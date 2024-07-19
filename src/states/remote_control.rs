use std::cell::Cell;
use std::fmt::{Display, Formatter};
use std::future::{pending};
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;
use bluefang::avc::PassThroughOp;
use bluefang::avrcp;
use bluefang::avrcp::{AvrcpSession, Event, MediaAttributeId, Notification};
use bluefang::avrcp::notifications::{CurrentTrack, PlaybackPosition, PlaybackStatus};
use bluefang::utils::{Either2, IgnoreableResult, select2};
use iced::{Alignment, Command, Element, Font, Length, Renderer, Subscription, Theme};
use iced::font::Weight;
use iced::futures::{SinkExt};
use iced::futures::channel::mpsc::Sender as IcedSender;
use iced::widget::{button, Column, Row, Space, text};
use iced::window::Id;
use iced::window::raw_window_handle::RawWindowHandle;
use portable_atomic::AtomicF32;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};
use tokio::sync::mpsc::Sender;
use tracing::{error, info, trace, warn};
use crate::icon;
use crate::states::SubState;

#[derive(Debug)]
pub enum Message {
    MediaControlsInitialized(Option<MediaControls>),
    MediaCommandChannel(Sender<RemoteControlCommand>),
    TrackChanged(Option<MediaInfo>),
    PlaybackStatusChanged(PlaybackStatus),
    PlaybackPositionChanged(Option<Duration>),
    Button(PassThroughOp),
    VolumeChanged
}

#[derive(Default, Debug, Clone)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub duration: Duration
}

impl MediaInfo {
    pub fn validate(self) -> Option<Self> {
        if self.title.is_empty() && self.artist.is_empty() && self.duration.is_zero() {
            None
        } else {
            Some(self)
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum RemoteControlCommand {
    Op(PassThroughOp),
    VolumeChanged
}

pub struct RemoteControlSession {
    session: Cell<Option<AvrcpSession>>,
    volume: Arc<AtomicF32>,
    media_controls: Option<MediaControls>,
    media_commands: Option<Sender<RemoteControlCommand>>,
    info: Option<MediaInfo>,
    position: Option<Duration>,
    status: PlaybackStatus
}

impl RemoteControlSession {
    pub fn new(session: AvrcpSession, volume: Arc<AtomicF32>) -> (Self, Command<Message>) {
        let state = Self {
            session: Cell::new(Some(session)),
            volume,
            media_controls: None,
            media_commands: None,
            info: None,
            position: None,
            status: PlaybackStatus::Stopped,
        };
        (state, Command::none())
    }

    fn update_media_status(&mut self) {
        if let Some(controls) = &mut self.media_controls {
            controls.set_playback(match self.status {
                PlaybackStatus::Stopped | PlaybackStatus::Error => MediaPlayback::Stopped,
                PlaybackStatus::Playing | PlaybackStatus::FwdSeek | PlaybackStatus::RevSeek => MediaPlayback::Playing { progress: self.position.map(MediaPosition) },
                PlaybackStatus::Paused => MediaPlayback::Paused { progress: self.position.map(MediaPosition) },
            }).unwrap_or_else(|err| warn!("Failed to set playback status: {:?}", err));
        }
    }

    fn update_media_info(&mut self) {
        if let Some(controls) = &mut self.media_controls {
            controls.set_metadata(MediaMetadata {
                title: self.info.as_ref().map(|info| info.title.as_str()),
                album: None,
                artist: self.info.as_ref().map(|info| info.artist.as_str()),
                cover_url: None,
                duration: self.info.as_ref().map(|info| info.duration)
            }).unwrap_or_else(|err| warn!("Failed to set metadata: {:?}", err));
        }
    }

    pub fn notify_volume_change(&self) {
        if let Some(channel) = &self.media_commands  {
            channel.try_send(RemoteControlCommand::VolumeChanged).ignore();
        }
    }

}

impl SubState for RemoteControlSession {
    type Message = Message;

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Message::MediaCommandChannel(sender) => {
                self.media_commands = Some(sender);
                iced::window::run_with_handle(Id::MAIN, |handle| {
                    let hwnd = match handle.as_raw() {
                        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as _),
                        _ => None
                    };
                    let controls = MediaControls::new(PlatformConfig {
                        display_name: "Bluefang",
                        dbus_name: "bluefang",
                        hwnd,
                    });
                    if let Err(err) = &controls {
                        error!("Failed to initialize media controls: {:?}", err)
                    }
                    Message::MediaControlsInitialized(controls.ok())
                })
            }
            Message::MediaControlsInitialized(controls) => {
                self.media_controls = controls;
                if let Some(controls) = &mut self.media_controls {
                    let channel = self.media_commands.clone().expect("Media command channel not set");
                    controls.attach(move |event| {
                        let cmd = match event {
                            MediaControlEvent::Play => Some(PassThroughOp::Play),
                            MediaControlEvent::Pause => Some(PassThroughOp::Pause),
                            MediaControlEvent::Previous => Some(PassThroughOp::Backward),
                            MediaControlEvent::Next => Some(PassThroughOp::Forward),
                            _ => None
                        };
                        if let Some(cmd) = cmd {
                            channel.try_send(RemoteControlCommand::Op(cmd)).ignore();
                        }
                    }).unwrap_or_else(|err| warn!("Failed to attach event handler: {:?}", err));
                }
                Command::none()
            }
            Message::TrackChanged(info) => {
                self.info = info;
                self.update_media_info();
                Command::none()
            }
            Message::PlaybackStatusChanged(status) => {
                self.status = status;
                self.update_media_status();
                Command::none()
            }
            Message::PlaybackPositionChanged(pos) => {
                self.position = pos;
                self.update_media_status();
                Command::none()
            }
            Message::Button(op) => {
                if let Some(channel) = &self.media_commands {
                    channel.try_send(RemoteControlCommand::Op(op)).ignore();
                }
                Command::none()
            }
            Message::VolumeChanged => Command::none()
        }
    }

    fn  view<'a>(&self) -> Element<'a, Self::Message, Theme, Renderer> {
        let (enabled, middle) = match self.status {
            PlaybackStatus::Stopped | PlaybackStatus::Error => (false, ('\u{e71c}', PassThroughOp::Play)),
            PlaybackStatus::FwdSeek | PlaybackStatus::Playing | PlaybackStatus::RevSeek => (true, ('\u{e71d}', PassThroughOp::Pause)),
            PlaybackStatus::Paused => (true, ('\u{e71c}', PassThroughOp::Play))
        };
        let controls = Row::new()
            .spacing(30.0)
            .push(Space::new(Length::FillPortion(1), Length::Shrink))
            .extend([('\u{e774}', PassThroughOp::Backward), middle, ('\u{e775}', PassThroughOp::Forward)]
                .into_iter()
                .map(|(i, e)| button(icon(i)
                    .size(20.0)
                    .width(Length::Fixed(50.0)))
                    .on_press_maybe(enabled.then_some(e)))
                .map(Element::from))
            .push(Space::new(Length::FillPortion(1), Length::Shrink));

        let current = self.position.unwrap_or_default();
        let end = self.info.as_ref().and_then(|info| (!info.duration.is_zero()).then_some(info.duration));
        let end_time = end.map(|time| Timestamp::new(time, false));
        let current_time = end_time.map(|end| Timestamp::new(current, end.has_hours()));
        let fraction = end.map_or(0.0, |end| current.as_secs_f64() / end.as_secs_f64()) as f32;

        let progress = Row::new()
            .align_items(Alignment::Center)
            .spacing(10)
            .push(text(current_time.unwrap_or_default()))
            .push(iced::widget::progress_bar(0.0..=1.0, fraction))
            .push(text(end_time.unwrap_or_default()));
        let info = Column::new()
            .padding([10, 20])
            .push(text(self.info.as_ref().map(|info| info.artist.as_str()).unwrap_or_default()))
            .push(text(self.info.as_ref().map(|info| info.title.as_str()).unwrap_or("No Track Selected"))
                .font(Font {
                    weight: Weight::Bold,
                    ..Default::default()
                })
                .size(25));

        let bottom_bar = Column::new()
            .padding(10)
            .spacing(10)
            .push(Space::new(Length::Shrink, Length::Fill))
            .push(info)
            .push(progress)
            .push(controls);

        Element::from(bottom_bar)
            .map(Message::Button)
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let session = self.session.take();
        let volume = self.volume.clone();
        #[derive(Hash)]
        struct Id;
        iced::subscription::channel(Id, 100, |mut output| async move {
            let mut session = session.expect("session already taken");
            let (tx, mut commands) = tokio::sync::mpsc::channel(20);
            let _ = output.send(Message::MediaCommandChannel(tx)).await;
            session
                .notify_local_volume_change(volume.load(SeqCst))
                .await
                .unwrap_or_else(|err| warn!("Failed to notify volume change: {}", err));
            let supported_events = session.get_supported_events().await.unwrap_or_default();
            info!("Supported Events: {:?}", supported_events);
            if supported_events.contains(&CurrentTrack::EVENT_ID) {
                register_current_track_notification(&session, &mut output).await;
            }
            if supported_events.contains(&PlaybackStatus::EVENT_ID) {
                register_playback_status_notification(&session, &mut output).await;
            }
            if supported_events.contains(&PlaybackPosition::EVENT_ID) {
                register_playback_position_notification(&session, &mut output).await;
            }
            loop {
                match select2(commands.recv(), session.next_event()).await {
                    Either2::A(Some(cmd)) => {
                        match cmd {
                            RemoteControlCommand::Op(op) => {
                                trace!("Sending command: {:?}", op);
                                match session.action(op).await {
                                    Ok(_) if matches!(op, PassThroughOp::Play | PassThroughOp::Pause) => {
                                        let playback_status= match op {
                                            PassThroughOp::Play => PlaybackStatus::Playing,
                                            PassThroughOp::Pause => PlaybackStatus::Paused,
                                            _ => unreachable!()
                                        };
                                        let _ = output.send(Message::PlaybackStatusChanged(playback_status)).await;
                                    }
                                    Err(err) => warn!("Failed to send action: {}", err),
                                    _ => {}
                                }
                            }
                            RemoteControlCommand::VolumeChanged => {
                                session.notify_local_volume_change(volume.load(SeqCst))
                                    .await
                                    .unwrap_or_else(|err| warn!("Failed to notify volume change: {}", err));
                            }
                        }
                    }
                    Either2::B(Some(event)) => match event {
                        Event::TrackChanged(_) => {
                            register_current_track_notification(&session, &mut output).await;
                        }
                        Event::PlaybackStatusChanged(_) => {
                            register_playback_status_notification(&session, &mut output).await;
                        }
                        Event::PlaybackPositionChanged(_) => {
                            register_playback_position_notification(&session, &mut output).await;
                        }
                        Event::VolumeChanged(vol) => {
                            volume.store(vol, SeqCst);
                            let _ = output.send(Message::VolumeChanged).await;
                        }
                    },
                    _ => break
                }
            }
            pending().await
        })
    }
}



async fn register_playback_status_notification(session: &AvrcpSession, output: &mut IcedSender<Message>) {
    let playback_status: PlaybackStatus = session.register_notification(None).await
        .unwrap_or_else(|err| {
            warn!("Failed to register playback status notification: {}", err);
            PlaybackStatus::Stopped
        });
    let _ = output.send(Message::PlaybackStatusChanged(playback_status)).await;
}

async fn register_playback_position_notification(session: &AvrcpSession, output: &mut IcedSender<Message>) {
    let playback_position: PlaybackPosition = session.register_notification(Some(Duration::from_secs(1))).await
        .unwrap_or_else(|err| {
            warn!("Failed to register playback position notification: {}", err);
            Default::default()
        });
    let _ = output.send(Message::PlaybackPositionChanged(playback_position.as_option())).await;
}

async fn register_current_track_notification(session: &AvrcpSession, output: &mut IcedSender<Message>) {
    match retrieve_current_track_info(&session).await {
        Ok(msg) => {
            let _ = output.send(Message::TrackChanged(msg)).await;
        },
        Err(err) => warn!("Failed to retrieve current track info: {}", err)
    }
}

async fn retrieve_current_track_info(session: &AvrcpSession) -> Result<Option<MediaInfo>, avrcp::Error> {
    let current_track: CurrentTrack = session.register_notification(None).await?;
    Ok(match current_track {
        CurrentTrack::NotSelected | CurrentTrack::Id(_) => None,
        CurrentTrack::Selected => {
            let attributes = session
                .get_current_media_attributes(Some(&[MediaAttributeId::Title, MediaAttributeId::ArtistName, MediaAttributeId::PlayingTime]))
                .await?;
            MediaInfo {
                title: attributes
                    .get(&MediaAttributeId::Title)
                    .cloned()
                    .unwrap_or_else(String::new),
                artist: attributes
                    .get(&MediaAttributeId::ArtistName)
                    .cloned()
                    .unwrap_or_else(String::new),
                duration: attributes
                    .get(&MediaAttributeId::PlayingTime)
                    .and_then(|time| time.parse::<u64>().ok())
                    .map_or(Duration::ZERO, Duration::from_millis)
            }.validate()
        }
    })
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
struct Timestamp {
    hours: Option<u16>,
    minutes: u16,
    seconds: u16
}

impl Timestamp {

    pub fn new(time: Duration, force_hours: bool) -> Self {
        let mut seconds = time.as_secs();
        let mut minutes = seconds / 60;
        seconds -= minutes * 60;
        let hours = minutes / 60;
        minutes -= hours * 60;
        Self {
            hours: (hours > 0 || force_hours).then_some(hours as u16),
            minutes: minutes as u16,
            seconds: seconds as u16
        }
    }

    pub fn has_hours(&self) -> bool {
        self.hours.is_some()
    }
}

impl Display for Timestamp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.hours {
            Some(hours) => write!(f, "{:02}:{:02}:{:02}", hours, self.minutes, self.seconds),
            None => write!(f, "{:02}:{:02}", self.minutes, self.seconds)
        }
    }
}

#[test]
fn test_timestamp() {
    let info: Option<MediaInfo> = None;
    let current = Duration::from_secs(30);
    let end_time = info.as_ref().map(|info| Timestamp::new(info.duration, false));
    let current_time = end_time.map(|end| Timestamp::new(current, end.has_hours()));
    println!("Current Time: {}, End: {}", current_time.unwrap_or_default(), end_time.unwrap_or_default());
}