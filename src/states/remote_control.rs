use std::cell::Cell;
use std::future::pending;
use bluefang::avrcp;
use bluefang::avrcp::{AvrcpSession, Event, MediaAttributeId, Notification};
use bluefang::avrcp::notifications::CurrentTrack;
use iced::{Command, Element, Renderer, Subscription, Theme};
use iced::futures::SinkExt;
use iced::widget::{Column, text};
use tracing::{info, warn};
use crate::states::SubState;

#[derive(Debug)]
pub enum Message {
    TrackChanged {
        title: String,
        artist: String,
    },
}

pub struct RemoteControlSession {
    session: Cell<Option<AvrcpSession>>,
    pub title: String,
    pub artist: String,
}

impl RemoteControlSession {
    pub fn new(session: AvrcpSession) -> Self {
        Self {
            session: Cell::new(Some(session)),
            title: "".to_string(),
            artist: "".to_string(),
        }
    }
}

impl SubState for RemoteControlSession {
    type Message = Message;

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Message::TrackChanged { artist, title } => {
                self.artist = artist;
                self.title = title;
                Command::none()
            }
        }
    }

    fn  view<'a>(&self) -> Element<'a, Self::Message, Theme, Renderer> {
        Column::new()
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
            while let Some(event) = session.next_event().await {
                match event {
                    Event::TrackChanged(_) => {
                        match retrieve_current_track_info(&session).await {
                            Ok(msg) => {
                                let _ = output.send(msg).await;
                            },
                            Err(err) => warn!("Failed to retrieve current track info: {}", err)
                        }
                    }
                    Event::VolumeChanged(vol) => {
                        //volume.store(vol, SeqCst);
                        //println!("Volume: {}%", (volume.load(SeqCst) * 100.0).round());
                        println!("Volume: {}%", (vol * 100.0).round());
                    }
                }
            }
            pending().await
        })
    }
}

async fn retrieve_current_track_info(session: &AvrcpSession) -> Result<Message, avrcp::Error> {
    let current_track: CurrentTrack = session.register_notification().await?;
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