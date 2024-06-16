mod bluetooth;

use std::sync::Arc;
use bluefang::firmware::RealTekFirmwareLoader;
use bluefang::hci;
use bluefang::hci::{FirmwareLoader, Hci};
use iced::{Alignment, Application, Command, Element, Event, Renderer, Subscription, window};
use iced::event::{listen_with, Status};
use iced::widget::{button, Column, text};
use iced::window::{close, Id};
use tokio::time::sleep;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use crate::bluetooth::initialize_hci;

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

#[derive(Debug, Clone)]
enum Message {
    HciInitialized(Arc<Hci>),
    HciFailure(Arc<hci::Error>),
    CloseRequested,
    Increment,
    Decrement,
}

#[derive(Default)]
struct App {
    hci: Option<Arc<Hci>>,
    value: i64,
}

impl Application for App {
    type Executor = iced::executor::Default;
    type Message = Message;
    type Theme = iced::Theme;
    type Flags = ();

    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (Self {
            hci: None,
            value: 0,
        }, initialize_hci())
    }

    fn title(&self) -> String {
        "Bluefang Player".to_string()
    }

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Message::Increment => {
                self.value += 1;
                Command::perform(async { sleep(std::time::Duration::from_secs(1)).await; Message::Decrement }, |msg| msg)
            }
            Message::Decrement => {
                self.value -= 1;
                Command::none()
            },
            Message::HciFailure(e) => {
                panic!("HCI initialization failed: {:?}", e)
            },
            Message::HciInitialized(hci) => {
                self.hci = Some(hci);
                Command::none()
            },
            Message::CloseRequested => {
                match self.hci.take() {
                    Some(hci) => Command::perform(async move {
                        hci.shutdown().await
                            .unwrap_or_else(|e| tracing::error!("Failed to shut down HCI: {:?}", e));
                        Message::CloseRequested
                    }, |msg| msg),
                    None => close(Id::MAIN)
                }
            }
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        listen_with(|event, status| match (event, status) {
            (Event::Window(Id::MAIN, window::Event::CloseRequested), Status::Ignored) => Some(Message::CloseRequested),
            _ => None
        })
    }

    fn view(&self) -> Element<'_, Self::Message, Self::Theme, Renderer> {
        Column::new()
            .push(button("Increment").on_press(Message::Increment))
            .push(text(self.value).size(50))
            .push(button("Decrement").on_press(Message::Decrement))
            .padding(20)
            .align_items(Alignment::Center)
            .into()
    }

}

