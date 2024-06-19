use std::sync::Arc;

use bluefang::firmware::RealTekFirmwareLoader;
use bluefang::hci;
use bluefang::hci::{FirmwareLoader, Hci};
use iced::{Application, Command, Element, Event, Length, Renderer, Subscription, window};
use iced::alignment::{Horizontal, Vertical};
use iced::event::{listen_with, Status};
use iced::widget::{text, Text};
use iced::window::{close, Id};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::bluetooth::initialize_hci;
use crate::states::{SubState, Running};

mod bluetooth;
mod states;
mod audio;

fn main() -> iced::Result {
    tracing_subscriber::registry()
        .with(layer().without_time())
        .with(EnvFilter::from_default_env())
        .init();
    Hci::register_firmware_loaders([RealTekFirmwareLoader::new("firmware").boxed()]);
    App::run(iced::Settings {
        window: window::Settings {
            exit_on_close_request: false,
            ..Default::default()
        },
        ..Default::default()
    })
}

#[derive(Debug)]
enum Message {
    HciInitialized(Arc<Hci>),
    HciFailure(hci::Error),
    CloseRequested,
    ShutdownCompleted,
    Running(<Running as SubState>::Message)
}

struct App {
    state: State
}

enum State {
    Initializing,
    Running(Running, bool),
    Failed(hci::Error)
}

impl Application for App {
    type Executor = iced::executor::Default;
    type Message = Message;
    type Theme = iced::Theme;
    type Flags = ();

    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (Self {
            state: State::Initializing,//State::Failed(Arc::new(hci::Error::Generic("HCI not initialized")))
        }, initialize_hci())
    }

    fn title(&self) -> String {
        "Bluefang Player".to_string()
    }

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Message::HciFailure(e) => {
                self.state = State::Failed(e);
                Command::none()
            },
            Message::HciInitialized(hci) => {
                assert!(matches!(&self.state, State::Initializing | State::Failed(_)));
                let (state, cmd) = Running::new(hci.clone());
                self.state = State::Running(state, true);
                cmd.map(Message::Running)
            },
            Message::CloseRequested => {
                match &mut self.state {
                    State::Running(running, active) => match active {
                        true => {
                            *active = false;
                            Command::perform(running.shutdown(), |_| Message::ShutdownCompleted)
                        },
                        false => Command::none()
                    },
                    _ => close(Id::MAIN)
                }
            }
            Message::Running(msg) => match &mut self.state {
                State::Running(running, _) => running
                    .update(msg)
                    .map(Message::Running),
                _ => Command::none()
            },
            Message::ShutdownCompleted => close(Id::MAIN)
        }
    }

    fn view(&self) -> Element<'_, Self::Message, Self::Theme, Renderer> {
        match &self.state {
            State::Initializing => centered_text("Initializing...").into(),
            State::Failed(e) => centered_text(format!("Failed to initialize HCI: {}", e)).into(),
            State::Running(_, false) => centered_text("Shutting down...").into(),
            State::Running(running, _) => running.view().map(Message::Running)
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let running_events = match &self.state {
            State::Running(running, _) => running
                .subscription()
                .map(Message::Running),
            _ => Subscription::none()
        };
        let close_events = listen_with(|event, status| match (event, status) {
            (Event::Window(Id::MAIN, window::Event::CloseRequested), Status::Ignored) => Some(Message::CloseRequested),
            _ => None
        });
        Subscription::batch([close_events, running_events])
    }

}

pub fn centered_text<'a, Theme>(text: impl ToString) -> Text<'a, Theme, Renderer>
    where
        Theme: text::StyleSheet
{
    Text::new(text.to_string())
        .width(Length::Fill)
        .height(Length::Fill)
        .vertical_alignment(Vertical::Center)
        .horizontal_alignment(Horizontal::Center)
}

#[macro_export]
macro_rules! cloned {
    ([$($vars:ident),+] $e:expr) => {
        {
            $( let $vars = $vars.clone(); )+
            $e
        }
    };
}