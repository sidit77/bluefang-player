use std::collections::BTreeMap;
use std::fmt::Write;
use std::future::{pending, ready, Future};
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use bluefang::a2dp::sbc::SbcMediaCodecInformation;
use bluefang::a2dp::sdp::A2dpSinkServiceRecord;
use bluefang::avdtp::capabilities::Capability;
use bluefang::avdtp::{Avdtp, AvdtpBuilder, LocalEndpoint, MediaType, StreamEndpointType, StreamHandlerFactory};
use bluefang::avrcp::sdp::{AvrcpControllerServiceRecord, AvrcpTargetServiceRecord};
use bluefang::avrcp::{Avrcp, AvrcpSession};
use bluefang::hci;
use bluefang::hci::connection::{ConnectionEvent, ConnectionEventReceiver};
use bluefang::hci::consts::{AuthenticationRequirements, IoCapability, LinkKey, LinkType, OobDataPresence, RemoteAddr, Role, Status as HciStatus};
use bluefang::hci::{Hci, PageScanRepititionMode};
use bluefang::l2cap::L2capServerBuilder;
use bluefang::sdp::SdpBuilder;
use bluefang::utils::{select2, Either2, IgnoreableResult};
use iced::advanced::graphics::futures::{BoxStream, MaybeSend};
use iced::advanced::subscription::{EventStream, Recipe};
use iced::advanced::Hasher;
use iced::border::Radius;
use iced::futures::future::join;
use iced::futures::stream::{empty, once};
use iced::futures::StreamExt;
use iced::widget::container::Appearance;
use iced::widget::{button, text, Column, Row};
use iced::{Border, Color, Command, Element, Length, Renderer, Subscription, Theme};
use once_cell::sync::Lazy;
use portable_atomic::AtomicF32;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tracing::{debug, error, trace, warn};

use crate::audio::SbcStreamHandler;
use crate::states::remote_control::RemoteControlSession;
use crate::states::SubState;
use crate::{centered_text, cloned, icon, PROJECT_DIRS, RON_CONFIG};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedDevice {
    pub addr: RemoteAddr,
    pub name: Option<String>,
    pub link_key: LinkKey,
    pub last_connected: u64
}

impl PairedDevice {
    pub fn new(addr: RemoteAddr) -> Self {
        Self {
            addr,
            name: None,
            link_key: LinkKey::default(),
            last_connected: 0
        }
    }

    pub fn name_or_addr(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.addr.to_string())
    }

    pub fn valid(&self) -> bool {
        self.link_key != LinkKey::default()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
enum ConnectionState {
    #[default]
    Disconnected,
    Connecting(RemoteAddr, bool),
    Connected(RemoteAddr)
}

impl ConnectionState {
    pub fn addr(self) -> Option<RemoteAddr> {
        match self {
            ConnectionState::Connecting(addr, _) | ConnectionState::Connected(addr) => Some(addr),
            _ => None
        }
    }

    pub fn is_reconnecting(self) -> bool {
        match self {
            ConnectionState::Connecting(_, rc) => rc,
            _ => {
                warn!("is_reconnecting called on {:?}", self);
                false
            }
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum ButtonEvent {
    ModifyVolume(f32),
    ReconnectTo(RemoteAddr)
}

#[derive(Debug)]
pub enum Message {
    ConnectionEvent(ConnectionEvent),
    PairedDevicesLoaded(Vec<PairedDevice>),
    NewAvrcpSession(AvrcpSession),
    RemoteControlEvent(<RemoteControlSession as SubState>::Message),
    ServersStarted(UnboundedSender<ServerCommand>),
    Error(Arc<hci::Error>),
    Button(ButtonEvent)
}

#[derive(Debug, Copy, Clone)]
pub enum ServerCommand {
    ConnectTo(u16)
}

pub struct Running {
    hci: Arc<Hci>,
    connection_state: ConnectionState,
    paired_devices: BTreeMap<RemoteAddr, PairedDevice>,
    volume: Arc<AtomicF32>,
    remote_control_session: Option<RemoteControlSession>,
    server_command_sender: Option<UnboundedSender<ServerCommand>>
}

static PAIRED_DEVICES_PATH: Lazy<PathBuf> = Lazy::new(|| PROJECT_DIRS.data_dir().join("paired-devices.ron"));

impl Running {
    pub fn new(hci: Arc<Hci>) -> (Self, Command<Message>) {
        let state = Self {
            hci,
            connection_state: ConnectionState::Disconnected,
            paired_devices: Default::default(),
            volume: Arc::new(AtomicF32::new(1.0)),
            remote_control_session: None,
            server_command_sender: None
        };
        let commands = Command::batch([Command::perform(load_paired_devices(), |r| match r {
            Ok(link_keys) => Message::PairedDevicesLoaded(link_keys),
            Err(e) => Message::Error(Arc::new(e))
        })]);
        (state, commands)
    }

    pub fn shutdown(&self) -> impl Future<Output = ()> + 'static {
        let hci = self.hci.clone();
        let keys = self
            .paired_devices
            .values()
            .cloned()
            .filter(PairedDevice::valid)
            .collect();
        async move {
            hci.shutdown()
                .await
                .unwrap_or_else(|e| tracing::error!("Failed to shut down HCI: {:?}", e));
            save_link_keys(&keys)
                .await
                .unwrap_or_else(|e| tracing::error!("Failed to save link keys: {:?}", e));
        }
    }
}

impl SubState for Running {
    type Message = Message;

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Message::ConnectionEvent(event) => self.handle_connection_event(event),
            Message::Error(err) => {
                error!("Error: {:?}", err);
                Command::none()
            }
            Message::PairedDevicesLoaded(devices) => {
                for device in devices {
                    self.paired_devices.entry(device.addr).or_insert(device);
                }
                Command::none()
            }
            Message::NewAvrcpSession(session) => {
                let (state, cmd) = RemoteControlSession::new(session, self.volume.clone());
                self.remote_control_session = Some(state);
                cmd.map(Message::RemoteControlEvent)
            }
            Message::RemoteControlEvent(e) => match &mut self.remote_control_session {
                Some(rcs) => rcs.update(e).map(Message::RemoteControlEvent),
                None => Command::none()
            },
            Message::Button(e) => match e {
                ButtonEvent::ModifyVolume(d) => {
                    let _ = self
                        .volume
                        .fetch_update(SeqCst, SeqCst, |v| Some((v + d).max(0.0).min(1.0)));
                    if let Some(control) = &self.remote_control_session {
                        control.notify_volume_change();
                    }
                    Command::none()
                }
                ButtonEvent::ReconnectTo(addr) => {
                    self.connection_state = ConnectionState::Connecting(addr, true);
                    self.call(|hci| async move { hci.create_connection(addr, true).await })
                }
            },
            Message::ServersStarted(sender) => {
                self.server_command_sender = Some(sender);
                Command::none()
            }
        }
    }

    fn view<'a>(&self) -> Element<'a, Self::Message, Theme, Renderer> {
        //Column::new()
        //    .push(text(format!("Connection state: {:?}", self.connection_state)))
        //    .push(text(format!("Volume: {:?}", (self.volume.load(Ordering::Relaxed) * 100.0).round())))
        //    .push(self.remote_control_session.as_ref().map_or_else(
        //        || text("No remote control session").into(),
        //        |rcs| rcs.view().map(Message::RemoteControlEvent)
        //    ))
        //    .padding(30.0)
        //    .into()

        let mut status = match self.connection_state {
            ConnectionState::Disconnected => "Disconnected",
            ConnectionState::Connecting { .. } => "Connecting",
            ConnectionState::Connected { .. } => "Connected"
        }
        .to_string();
        if let Some(device) = self
            .connection_state
            .addr()
            .and_then(|a| self.paired_devices.get(&a))
        {
            write!(status, " to {}", device.name_or_addr()).unwrap();
        }
        let mut connection_bar = Row::new()
            .push(text(status).size(20).width(Length::Fill))
            .spacing(10);

        if let ConnectionState::Connected { .. } = self.connection_state {
            let volume = self.volume.load(Ordering::Relaxed);
            let volume_control: Element<_, _, _> = Row::new()
                .spacing(3)
                .width(Length::Fixed(300.0))
                .push(button(icon('\u{e71f}')).on_press_maybe((volume > 0.0).then_some(ButtonEvent::ModifyVolume(-0.05))))
                .push(iced::widget::progress_bar(0.0..=1.0, volume))
                .push(button(icon('\u{e721}')).on_press_maybe((volume < 1.0).then_some(ButtonEvent::ModifyVolume(0.05))))
                .into();

            connection_bar = connection_bar.push(volume_control.map(Message::Button));
        }

        if let ConnectionState::Disconnected { .. } = self.connection_state {
            if let Some(device) = self
                .paired_devices
                .values()
                .min_by_key(|d| d.last_connected)
            {
                let reconnect_button: Element<_, _, _> = button(text(format!("Reconnect to {}", device.name_or_addr())))
                    .on_press_maybe(
                        self.server_command_sender
                            .is_some()
                            .then_some(ButtonEvent::ReconnectTo(device.addr))
                    )
                    .into();
                connection_bar = connection_bar.push(reconnect_button.map(Message::Button));
            }
        }

        Column::new()
            .push(connection_bar)
            .push(
                iced::widget::container::Container::new(self.remote_control_session.as_ref().map_or_else(
                    || centered_text("No remote control session").into(),
                    |rcs| rcs.view().map(Message::RemoteControlEvent)
                ))
                .style(Appearance {
                    //background: Some(Gradient::from(Linear::new(std::f32::consts::PI)
                    //    .add_stop(0.0, Color::from_rgba(0.0, 0.0, 0.0, 0.0))
                    //    .add_stop(1.0, Color::from_rgba(0.0, 0.0, 0.0, 0.3))
                    //).into()),
                    background: Some(Color::from_rgba8(0, 161, 96, 0.3).into()),
                    border: Border {
                        color: Color::from_rgba(0.0, 0.0, 0.0, 0.3),
                        width: 1.0,
                        radius: Radius::from(5.0)
                    },
                    ..Default::default()
                })
                .width(Length::Fill)
                .height(Length::Fill)
            )
            .spacing(10)
            .padding(10.0)
            .into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch([
            ConnectionEventWatcher::new(&self.hci).map(Message::ConnectionEvent),
            self.create_l2cap_servers(),
            self.remote_control_session
                .as_ref()
                .map(RemoteControlSession::subscription)
                .unwrap_or_else(Subscription::none)
                .map(Message::RemoteControlEvent)
        ])
    }
}

impl Running {
    fn create_l2cap_servers(&self) -> Subscription<Message> {
        #[derive(Hash)]
        struct Id;

        let hci = self.hci.clone();
        let volume = self.volume.clone();
        iced::subscription::channel(Id, 10, move |mut output| async move {
            trace!("Spawning L2CAP servers");
            let avdtp: Arc<Avdtp> = AvdtpBuilder::default()
                .with_endpoint(LocalEndpoint {
                    media_type: MediaType::Audio,
                    seid: 1,
                    in_use: Arc::new(AtomicBool::new(false)),
                    tsep: StreamEndpointType::Sink,
                    capabilities: vec![
                        Capability::MediaTransport,
                        Capability::MediaCodec(SbcMediaCodecInformation::default().into()),
                    ],
                    //stream_handler_factory: Box::new(|cap| Box::new(FileDumpHandler::new())),
                    factory: StreamHandlerFactory::new(cloned!([volume] move |cap| SbcStreamHandler::new(volume.clone(), cap)))
                })
                .build()
                .into();
            let mut l2cap_server = L2capServerBuilder::default()
                .with_protocol(
                    SdpBuilder::default()
                        .with_record(A2dpSinkServiceRecord::new(0x00010001))
                        .with_record(AvrcpControllerServiceRecord::new(0x00010002))
                        .with_record(AvrcpTargetServiceRecord::new(0x00010003))
                        .build()
                )
                .with_protocol(Avrcp::new(cloned!([output]
                    move |session| output
                        .try_send(Message::NewAvrcpSession(session))
                        .unwrap_or_else(|e| error!("Failed to send new Avrcp session: {:?}", e)))))
                .with_protocol(avdtp.clone())
                .run(&hci)
                .unwrap();
            let (tx, mut rx) = unbounded_channel();
            let server = async move {
                loop {
                    match select2(&mut l2cap_server, rx.recv()).await {
                        Either2::A(_) => {
                            trace!("L2CAP servers stopped");
                            break;
                        }
                        Either2::B(Some(cmd)) => match cmd {
                            ServerCommand::ConnectTo(handle) => {
                                debug!("Connecting AVDTP to handle: {}", handle);
                                avdtp.clone().connect(&mut l2cap_server, handle);
                            }
                        },
                        Either2::B(None) => {}
                    };
                }
            };
            let finish_setup = async move {
                hci.set_scan_enabled(true, true)
                    .await
                    .unwrap_or_else(|e| error!("Failed to enable scan: {:?}", e));
                output
                    .try_send(Message::ServersStarted(tx))
                    .unwrap_or_else(|e| error!("Failed to send server command sender: {:?}", e));
            };
            join(server, finish_setup).await;
            pending().await
        })
    }
}

impl Running {
    fn handle_connection_event(&mut self, event: ConnectionEvent) -> Command<Message> {
        tracing::debug!("Connection event: {:?}", event);
        match event {
            ConnectionEvent::ConnectionComplete { status, addr, handle, .. } => {
                let reconnecting = self.connection_state.is_reconnecting();
                self.connection_state = match status {
                    HciStatus::Success => ConnectionState::Connected(addr),
                    _ => ConnectionState::Disconnected
                };
                self.paired_devices
                    .entry(addr)
                    .or_insert_with(|| PairedDevice::new(addr))
                    .last_connected = current_unix_time();
                if reconnecting {
                    let Some(sender) = self.server_command_sender.clone() else {
                        error!("Server command sender not available");
                        return Command::none();
                    };
                    self.call(|hci| async move {
                        let role = hci.discover_role(handle).await?;
                        if role == Role::Master {
                            debug!("Requesting role switch");
                            if let Err(e) = hci.switch_role(addr, Role::Slave).await {
                                warn!("Failed to switch role: {:?}", e)
                            }
                        }
                        debug!("Requesting authentication");
                        hci.request_authentication(handle).await?;
                        debug!("Enabling encryption");
                        hci.set_encryption(handle, true).await?;
                        sender.send(ServerCommand::ConnectTo(handle)).ignore();
                        Ok(())
                    })
                } else {
                    Command::none()
                }
            }
            ConnectionEvent::DisconnectionComplete { .. } => {
                self.connection_state = ConnectionState::Disconnected;
                Command::none()
            }
            ConnectionEvent::ConnectionRequest { addr, link_type, .. } => {
                if link_type == LinkType::Acl && self.connection_state == ConnectionState::Disconnected {
                    self.connection_state = ConnectionState::Connecting(addr, false);
                    Command::batch([
                        self.call(|hci| async move { hci.accept_connection_request(addr, Role::Slave).await }),
                        self.call(|hci| async move {
                            hci.request_remote_name(addr, PageScanRepititionMode::R1)
                                .await
                        })
                    ])
                } else {
                    self.call(|hci| async move {
                        hci.reject_connection_request(addr, HciStatus::ConnectionRejectedDueToLimitedResources)
                            .await
                    })
                }
            }
            ConnectionEvent::RemoteNameRequestComplete { addr, name, status } => {
                if status != HciStatus::Success {
                    debug!("Remote name request failed: {:?}", status);
                    return Command::none();
                }
                self.paired_devices
                    .entry(addr)
                    .or_insert_with(|| PairedDevice::new(addr))
                    .name = Some(name);
                Command::none()
            }
            ConnectionEvent::PinCodeRequest { addr } => self.call(|hci| async move { hci.pin_code_request_reply(addr, "0000").await }),
            ConnectionEvent::LinkKeyRequest { addr } => {
                let key = self.paired_devices.get(&addr).map(|d| d.link_key.clone());
                if let Some(key) = key {
                    self.call(|hci| async move { hci.link_key_present(addr, &key).await })
                } else {
                    self.call(|hci| async move { hci.link_key_not_present(addr).await })
                }
            }
            ConnectionEvent::LinkKeyNotification { addr, key, .. } => {
                self.paired_devices
                    .entry(addr)
                    .or_insert_with(|| PairedDevice::new(addr))
                    .link_key = key;
                Command::none()
            }
            ConnectionEvent::IoCapabilityRequest { addr } => self.call(|hci| async move {
                hci.io_capability_reply(
                    addr,
                    IoCapability::NoInputNoOutput,
                    OobDataPresence::NotPresent,
                    AuthenticationRequirements::DedicatedBondingProtected
                )
                .await
            }),
            ConnectionEvent::UserConfirmationRequest { addr, .. } => self.call(|hci| async move { hci.user_confirmation_request_accept(addr).await }),
            ConnectionEvent::IoCapabilityResponse { .. }
            | ConnectionEvent::SimplePairingComplete { .. }
            | ConnectionEvent::LinkSuperVisionTimeoutChanged { .. }
            | ConnectionEvent::EncryptionChanged { .. } => Command::none(),
            other => {
                warn!("Event not supported: {:?}", other);
                Command::none()
            }
        }
    }

    fn call<A, F, T>(&self, f: A) -> Command<Message>
    where
        A: FnOnce(Arc<Hci>) -> F,
        F: Future<Output = Result<T, hci::Error>> + MaybeSend + 'static
    {
        let hci = self.hci.clone();
        let stream = once(f(hci)).filter_map(|r| ready(r.err().map(|e| Message::Error(Arc::new(e)))));
        Command::run(stream, |x| x)
    }
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn load_paired_devices() -> Result<Vec<PairedDevice>, hci::Error> {
    match tokio::fs::read_to_string(PAIRED_DEVICES_PATH.as_path()).await {
        Ok(data) => ron::de::from_str(&data).map_err(|_| hci::Error::Generic("Failed to parse paired devices")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err.into())
    }
}

async fn save_link_keys(paired_devices: &Vec<PairedDevice>) -> Result<(), hci::Error> {
    let text = ron::ser::to_string_pretty(&paired_devices, RON_CONFIG.clone()).unwrap();
    tokio::fs::create_dir_all(PAIRED_DEVICES_PATH.as_path().parent().unwrap()).await?;
    tokio::fs::write(PAIRED_DEVICES_PATH.as_path(), text).await?;
    Ok(())
}

struct ConnectionEventWatcher {
    hci: Arc<Hci>
}

impl ConnectionEventWatcher {
    fn new(hci: &Arc<Hci>) -> Subscription<ConnectionEvent> {
        Subscription::from_recipe(Self { hci: hci.clone() })
    }
}

impl Recipe for ConnectionEventWatcher {
    type Output = ConnectionEvent;

    fn hash(&self, state: &mut Hasher) {
        #[derive(Hash)]
        struct Id;
        Id.hash(state);
    }

    fn stream(self: Box<Self>, _: EventStream) -> BoxStream<Self::Output> {
        ConnectionEventReceiver::new(&self.hci)
            .map(|c| c.boxed())
            .unwrap_or_else(|e| {
                warn!("Failed to create connection event receiver: {:?}", e);
                empty().boxed()
            })
    }
}
