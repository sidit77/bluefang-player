use std::cell::Cell;
use std::fmt::{Display, Formatter};
use std::future::{pending};
use std::time::Duration;
use bluefang::avc::PassThroughOp;
use bluefang::avrcp;
use bluefang::avrcp::{AvrcpSession, Event, MediaAttributeId, Notification};
use bluefang::avrcp::notifications::{CurrentTrack, PlaybackStatus};
use bluefang::utils::{Either2, ResultExt, select2};
use iced::{Alignment, Command, Element, Font, Length, Renderer, Subscription, Theme};
use iced::font::Weight;
use iced::futures::{SinkExt};
use iced::widget::{button, Column, Row, Space, text};
use iced::window::Id;
use iced::window::raw_window_handle::RawWindowHandle;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
use tokio::sync::mpsc::Sender;
use tracing::{error, info, trace, warn};
use crate::icon;
use crate::states::SubState;

#[derive(Debug)]
pub enum Message {
    MediaControlsInitialized(Option<MediaControls>),
    MediaCommandChannel(Sender<PassThroughOp>),
    TrackChanged(Option<MediaInfo>),
    PlaybackStatusChanged(PlaybackStatus),
    Button(PassThroughOp)
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

pub struct RemoteControlSession {
    session: Cell<Option<AvrcpSession>>,
    media_controls: Option<MediaControls>,
    media_commands: Option<Sender<PassThroughOp>>,
    info: Option<MediaInfo>,
    status: PlaybackStatus
}

impl RemoteControlSession {
    pub fn new(session: AvrcpSession) -> (Self, Command<Message>) {
        let state = Self {
            session: Cell::new(Some(session)),
            media_controls: None,
            media_commands: None,
            info: None,
            status: PlaybackStatus::Stopped,
        };
        (state, Command::none())
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
                            channel.try_send(cmd).ignore();
                        }
                    }).unwrap_or_else(|err| warn!("Failed to attach event handler: {:?}", err));
                }
                Command::none()
            }
            Message::TrackChanged(info) => {
                if let Some(controls) = &mut self.media_controls {
                    controls.set_metadata(MediaMetadata {
                        title: info.as_ref().map(|info| info.title.as_str()),
                        album: None,
                        artist: info.as_ref().map(|info| info.artist.as_str()),
                        cover_url: None,
                        duration: None,
                    }).unwrap_or_else(|err| warn!("Failed to set metadata: {:?}", err));
                }
                self.info = info;
                Command::none()
            }
            Message::PlaybackStatusChanged(status) => {
                if let Some(controls) = &mut self.media_controls {
                    controls.set_playback(match status {
                        PlaybackStatus::Stopped | PlaybackStatus::Error => MediaPlayback::Stopped,
                        PlaybackStatus::Playing | PlaybackStatus::FwdSeek | PlaybackStatus::RevSeek => MediaPlayback::Playing { progress: None },
                        PlaybackStatus::Paused => MediaPlayback::Paused { progress: None },
                    }).unwrap_or_else(|err| warn!("Failed to set playback status: {:?}", err));
                }
                self.status = status;
                Command::none()
            }
            Message::Button(op) => {
                if let Some(channel) = &self.media_commands {
                    channel.try_send(op).ignore();
                }
                Command::none()
            }
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
        let current = Duration::from_secs(30);
        let end_time = self.info.as_ref().map(|info| Timestamp::new(info.duration, false));
        let current_time = end_time.map(|end| Timestamp::new(current, end.has_hours()));

        let progress = Row::new()
            .align_items(Alignment::Center)
            .spacing(10)
            .push(text(current_time.unwrap_or_default()))
            .push(iced::widget::progress_bar(0.0..=1.0, 0.5))
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
        #[derive(Hash)]
        struct Id;
        iced::subscription::channel(Id, 100, |mut output| async move {
            let mut session = session.expect("session already taken");
            let (tx, mut commands) = tokio::sync::mpsc::channel(20);
            let _ = output.send(Message::MediaCommandChannel(tx)).await;
            let supported_events = session.get_supported_events().await.unwrap_or_default();
            info!("Supported Events: {:?}", supported_events);
            if supported_events.contains(&CurrentTrack::EVENT_ID) {
                match retrieve_current_track_info(&session).await {
                    Ok(msg) => {
                        let _ = output.send(Message::TrackChanged(msg)).await;
                    },
                    Err(err) => warn!("Failed to retrieve current track info: {}", err)
                }
            }
            if supported_events.contains(&PlaybackStatus::EVENT_ID) {
                let playback_status: PlaybackStatus = session.register_notification(None).await
                    .unwrap_or_else(|err| {
                        warn!("Failed to register playback status notification: {}", err);
                        PlaybackStatus::Stopped
                    });
                let _ = output.send(Message::PlaybackStatusChanged(playback_status)).await;
            }
            loop {
                match select2(commands.recv(), session.next_event()).await {
                    Either2::A(Some(op)) => {
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
                    Either2::B(Some(event)) => match event {
                        Event::TrackChanged(_) => {
                            match retrieve_current_track_info(&session).await {
                                Ok(msg) => {
                                    let _ = output.send(Message::TrackChanged(msg)).await;
                                },
                                Err(err) => warn!("Failed to retrieve current track info: {}", err)
                            }
                        }
                        Event::PlaybackStatusChanged(_) => {
                            let playback_status: PlaybackStatus = session.register_notification(None).await
                                .unwrap_or_else(|err| {
                                    warn!("Failed to register playback status notification: {}", err);
                                    PlaybackStatus::Stopped
                                });
                            let _ = output.send(Message::PlaybackStatusChanged(playback_status)).await;
                        }
                        Event::VolumeChanged(vol) => {
                            //volume.store(vol, SeqCst);
                            //println!("Volume: {}%", (volume.load(SeqCst) * 100.0).round());
                            println!("Volume: {}%", (vol * 100.0).round());
                        }
                        _ => {}
                    },
                    _ => break
                }
            }
            pending().await
        })
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