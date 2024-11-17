mod remote_control;
mod running;

use iced::{Command, Element, Subscription};
pub use running::Running;

pub trait SubState: Sized {
    type Message: std::fmt::Debug + Send;
    fn update(&mut self, message: Self::Message) -> Command<Self::Message>;
    fn view<'a>(&self) -> Element<'a, Self::Message, iced::Theme, crate::Renderer>;
    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::none()
    }
}
