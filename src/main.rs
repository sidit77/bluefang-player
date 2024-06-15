use iced::{Alignment, Application, Command, Element, Renderer};
use iced::widget::{button, Column, text};
use tokio::time::sleep;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> iced::Result {
    tracing_subscriber::registry()
        .with(layer().without_time())
        .with(EnvFilter::from_default_env())
        .init();
    App::run(iced::Settings::default())
}

#[derive(Debug, Clone, Copy)]
enum Message {
    Increment,
    Decrement,
}

#[derive(Default)]
struct App {
    value: i64,
}

impl Application for App {
    type Executor = iced::executor::Default;
    type Message = Message;
    type Theme = iced::Theme;
    type Flags = ();

    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (Self {
            value: 0,
        }, Command::none())
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
            }
        }
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

