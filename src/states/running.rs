use std::collections::BTreeMap;
use std::future::{Future, ready};
use std::hash::Hash;
use std::path::Path;
use std::sync::Arc;
use bluefang::hci;
use bluefang::hci::connection::{ConnectionEvent, ConnectionEventReceiver};
use bluefang::hci::consts::{AuthenticationRequirements, IoCapability, LinkKey, LinkType, OobDataPresence, RemoteAddr, Role, Status as HciStatus};
use bluefang::hci::Hci;
use iced::{Command, Element, Renderer, Subscription, Theme};
use iced::advanced::graphics::futures::{BoxStream, MaybeSend};
use iced::advanced::Hasher;
use iced::advanced::subscription::{EventStream, Recipe};
use iced::futures::stream::{empty, once};
use iced::futures::{StreamExt};
use iced::widget::{Column, text};
use instructor::{Buffer, BufferMut};
use tracing::{error, warn};
use bytes::BytesMut;
use crate::states::SubState;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected
}

#[derive(Debug, Clone)]
pub enum Message {
    ConnectionEvent(ConnectionEvent),
    LinkKeysLoaded(BTreeMap<RemoteAddr, LinkKey>),
    Error(Arc<hci::Error>),
}

pub struct Running {
    hci: Arc<Hci>,
    connection_state: ConnectionState,
    link_keys: BTreeMap<RemoteAddr, LinkKey>
}

const LINK_KEYS_PATH: &str = "../bluefang/link-keys.dat";

impl Running {
    pub fn new(hci: Arc<Hci>) -> (Self, Command<Message>) {
        (Self {
            hci,
            connection_state: ConnectionState::Disconnected,
            link_keys: Default::default(),
        }, Command::perform(load_link_keys(LINK_KEYS_PATH), |r| match r {
            Ok(link_keys) => Message::LinkKeysLoaded(link_keys),
            Err(e) => Message::Error(Arc::new(e))
        }))
    }

    pub async fn shutdown(self)  {
        self.hci.shutdown().await
            .unwrap_or_else(|e| tracing::error!("Failed to shut down HCI: {:?}", e));
        save_link_keys(LINK_KEYS_PATH, &self.link_keys).await
            .unwrap_or_else(|e| tracing::error!("Failed to save link keys: {:?}", e));
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
            Message::LinkKeysLoaded(key) => {
                if !self.link_keys.is_empty() {
                    error!("Newer link keys already exist, ignoring loaded keys");
                } else {
                    self.link_keys = key;
                }
                Command::none()
            }
        }
    }

    fn view<'a>(&self) -> Element<'a, Self::Message, Theme, Renderer> {
        Column::new()
            .push(text(format!("Connection state: {:?}", self.connection_state)))
            .padding(30.0)
            .into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        ConnectionEventWatcher::new(&self.hci)
            .map(Message::ConnectionEvent)
    }
}

impl Running {

    fn handle_connection_event(&mut self, event: ConnectionEvent) -> Command<Message> {
        tracing::debug!("Connection event: {:?}", event);
        match event {
            ConnectionEvent::ConnectionComplete { status, .. } => {
                self.connection_state = match status {
                    HciStatus::Success => ConnectionState::Connected,
                    _ => ConnectionState::Disconnected
                };
                Command::none()
            },
            ConnectionEvent::DisconnectionComplete { .. } => {
                self.connection_state = ConnectionState::Disconnected;
                Command::none()
            },
            ConnectionEvent::ConnectionRequest { addr, link_type, .. } => {
                if link_type == LinkType::Acl && self.connection_state == ConnectionState::Disconnected {
                    self.connection_state = ConnectionState::Connecting;
                    self.call(|hci| async move { hci.accept_connection_request(addr, Role::Slave).await })
                } else {
                    self.call(|hci| async move { hci.reject_connection_request(addr, HciStatus::ConnectionRejectedDueToLimitedResources).await })
                }
            }
            ConnectionEvent::PinCodeRequest { addr} => {
                self.call(|hci| async move { hci.pin_code_request_reply(addr, "0000").await })
            }
            ConnectionEvent::LinkKeyRequest { addr} => {
                if let Some(key) = self.link_keys.get(&addr).cloned() {
                    self.call(|hci| async move { hci.link_key_present(addr, &key).await })
                } else {
                    self.call(|hci| async move { hci.link_key_not_present(addr).await })
                }
            },
            ConnectionEvent::LinkKeyNotification { addr, key, .. } => {
                self.link_keys.insert(addr, key);
                Command::none()
            },
            ConnectionEvent::IoCapabilityRequest { addr } => {
                self.call(|hci| async move { hci.io_capability_reply(
                    addr,
                    IoCapability::NoInputNoOutput,
                    OobDataPresence::NotPresent,
                    AuthenticationRequirements::DedicatedBondingProtected
                ).await })
            },
            ConnectionEvent::IoCapabilityResponse { .. } => Command::none(),
            ConnectionEvent::UserConfirmationRequest { addr, .. } => {
                self.call(|hci| async move { hci.user_confirmation_request_accept(addr).await })
            },
            ConnectionEvent::SimplePairingComplete { .. } => Command::none(),
            other => {
                warn!("Event not supported: {:?}", other);
                Command::none()
            }
        }
    }

    fn call<A, F, T>(&self, f: A) -> Command<Message>
        where
            A: FnOnce(Arc<Hci>) -> F,
            F: Future<Output = Result<T, hci::Error>> + MaybeSend + 'static,
    {
        let hci = self.hci.clone();
        let stream = once(f(hci))
            .filter_map(|r| ready(r
                .err()
                .map(|e| Message::Error(Arc::new(e)))));
        Command::run(stream, |x| x)
    }

}

async fn load_link_keys<P: AsRef<Path>>(path: P) -> Result<BTreeMap<RemoteAddr, LinkKey>, hci::Error> {
    match tokio::fs::read(path).await {
        Ok(data) => {
            let mut data = data.as_slice();
            let mut result = BTreeMap::new();
            while !data.is_empty() {
                let addr: RemoteAddr = data.read_le()?;
                let key: LinkKey = data.read_le()?;
                result.insert(addr, key);
            }
            Ok(result)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(err) => Err(err.into())
    }
}

async fn save_link_keys<P: AsRef<Path>>(path: P, link_keys: &BTreeMap<RemoteAddr, LinkKey>) -> Result<(), hci::Error> {
    let mut buf = BytesMut::new();
    for (addr, key) in link_keys {
        buf.write_le_ref(addr);
        buf.write_le_ref(key);
    }
    tokio::fs::write(path, buf).await?;
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