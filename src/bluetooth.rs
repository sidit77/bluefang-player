use std::sync::Arc;
use bluefang::hci::{Error, Hci};
use bluefang::hci::consts::{AudioVideoClass, ClassOfDevice, DeviceClass, MajorServiceClasses};
use bluefang::host::usb::{UsbController, UsbHost};
use iced::Command;
use tokio::task::spawn_blocking;
use tracing::info;
use crate::Message;

pub fn initialize_hci() -> Command<Message> {
    Command::perform(initialize_hci_internal(), |result| match result {
        Ok(hci) => Message::HciInitialized(hci),
        Err(e) => Message::HciFailure(Arc::new(e))
    })
}
async fn initialize_hci_internal() -> Result<Arc<Hci>, Error> {
    let usb = spawn_blocking::<_, Result<UsbHost, Error>>(|| {
        Ok(UsbController::list(|info| info.vendor_id() == 0x2B89 || info.vendor_id() == 0x10D7)?
            .next()
            .ok_or(Error::Generic("No compatible USB controller found"))?
            .claim()?)
    }).await.unwrap()?;

    let host = Arc::new(Hci::new(usb).await?);
    info!("Local BD_ADDR: {}", host.read_bd_addr().await?);

    let cod = ClassOfDevice {
        service_classes: MajorServiceClasses::Audio | MajorServiceClasses::Rendering,
        device_class: DeviceClass::AudioVideo(AudioVideoClass::WearableHeadset),
    };
    host.write_local_name("bluefang").await?;
    host.write_class_of_device(cod).await?;
    host.set_scan_enabled(true, true).await?;

    Ok(host)
}