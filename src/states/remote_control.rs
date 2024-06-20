use std::cell::Cell;
use std::future::{pending};
use bluefang::avc::PassThroughOp;
use bluefang::avrcp;
use bluefang::avrcp::{AvrcpSession, Event, MediaAttributeId, Notification};
use bluefang::avrcp::notifications::{CurrentTrack, PlaybackStatus};
use bluefang::utils::{Either2, ResultExt, select2};
use iced::{Command, Element, Renderer, Subscription, Theme};
use iced::futures::{SinkExt};
use iced::widget::{Column, text};
use iced::window::Id;
use iced::window::raw_window_handle::RawWindowHandle;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
use tokio::sync::mpsc::Sender;
use tracing::{error, info, trace, warn};
use crate::states::SubState;

#[derive(Debug)]
pub enum Message {
    MediaControlsInitialized(Option<MediaControls>),
    MediaCommandChannel(Sender<PassThroughOp>),
    TrackChanged {
        title: String,
        artist: String,
    },
    PlaybackStatusChanged(PlaybackStatus),
}

pub struct RemoteControlSession {
    session: Cell<Option<AvrcpSession>>,
    media_controls: Option<MediaControls>,
    media_commands: Option<Sender<PassThroughOp>>,
    pub title: String,
    pub artist: String,
    pub status: PlaybackStatus
}

impl RemoteControlSession {
    pub fn new(session: AvrcpSession) -> (Self, Command<Message>) {
        let state = Self {
            session: Cell::new(Some(session)),
            media_controls: None,
            media_commands: None,
            title: "".to_string(),
            artist: "".to_string(),
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
            Message::TrackChanged { artist, title } => {
                if let Some(controls) = &mut self.media_controls {
                    controls.set_metadata(MediaMetadata {
                        title: Some(&title),
                        album: None,
                        artist: Some(&artist),
                        cover_url: None,
                        duration: None,
                    }).unwrap_or_else(|err| warn!("Failed to set metadata: {:?}", err));
                }
                self.artist = artist;
                self.title = title;
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
        }
    }

    fn  view<'a>(&self) -> Element<'a, Self::Message, Theme, Renderer> {
        Column::new()
            .push(text(format!("Status: {:?}", self.status)))
            .push(text(format!("Artist: {}", self.artist)))
            .push(text(format!("Title: {}", self.title)))
            .into()
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
                        let _ = output.send(msg).await;
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
                        session.action(op).await.unwrap_or_else(|err| warn!("Failed to send action: {}", err));
                    }
                    Either2::B(Some(event)) => match event {
                        Event::TrackChanged(_) => {
                            match retrieve_current_track_info(&session).await {
                                Ok(msg) => {
                                    let _ = output.send(msg).await;
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

async fn retrieve_current_track_info(session: &AvrcpSession) -> Result<Message, avrcp::Error> {
    let current_track: CurrentTrack = session.register_notification(None).await?;
    match current_track {
        CurrentTrack::NotSelected | CurrentTrack::Id(_) => Ok(Message::TrackChanged {
            title: "".to_string(),
            artist: "".to_string(),
        }),
        CurrentTrack::Selected => {
            let attributes = session
                .get_current_media_attributes(Some(&[MediaAttributeId::Title, MediaAttributeId::ArtistName]))
                .await?;
            Ok(Message::TrackChanged {
                title: attributes
                    .get(&MediaAttributeId::Title)
                    .map_or("", String::as_str)
                    .to_string(),
                artist: attributes
                    .get(&MediaAttributeId::ArtistName)
                    .map_or("", String::as_str)
                    .to_string(),
            })
        }
    }
}
