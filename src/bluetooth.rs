use std::sync::Arc;
use bluefang::hci::{Error, Hci};
use bluefang::hci::consts::{AudioVideoClass, ClassOfDevice, DeviceClass, MajorServiceClasses};
use bluefang::host::usb::{UsbController, UsbHost};
use iced::Command;
use tokio::task::spawn_blocking;
use tracing::{info};
use crate::Message;

pub fn initialize_hci() -> Command<Message> {
    Command::perform(initialize_hci_internal(), |result| match result {
        Ok(hci) => Message::HciInitialized(hci),
        Err(e) => Message::HciFailure(e)
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
    host.set_simple_pairing_support(true).await?;
    host.write_local_name("bluefang").await?;
    host.write_class_of_device(cod).await?;

    Ok(host)
}
/*
fn avrcp_session_handler(volume: Arc<AtomicF32>, mut session: AvrcpSession) {
    spawn(async move {
        session
            .notify_local_volume_change(volume.load(SeqCst))
            .await
            .unwrap_or_else(|err| warn!("Failed to notify volume change: {}", err));
        sleep(Duration::from_millis(200)).await;
        let supported_events = session.get_supported_events().await.unwrap_or_default();
        info!("Supported Events: {:?}", supported_events);
        if supported_events.contains(&CurrentTrack::EVENT_ID) {
            retrieve_current_track_info(&session)
                .await
                .unwrap_or_else(|err| warn!("Failed to retrieve current track info: {}", err));
        }
        while let Some(event) = session.next_event().await {
            match event {
                Event::TrackChanged(_) => {
                    retrieve_current_track_info(&session)
                        .await
                        .unwrap_or_else(|err| warn!("Failed to retrieve current track info: {}", err));
                }
                Event::VolumeChanged(vol) => {
                    volume.store(vol, SeqCst);
                    println!("Volume: {}%", (volume.load(SeqCst) * 100.0).round());
                }
            }
        }
    });
}

async fn retrieve_current_track_info(session: &AvrcpSession) -> Result<(), avrcp::Error> {
    let current_track: CurrentTrack = session.register_notification().await?;
    match current_track {
        CurrentTrack::NotSelected => println!("No track selected"),
        CurrentTrack::Selected => {
            let attributes = session
                .get_current_media_attributes(Some(&[MediaAttributeId::Title, MediaAttributeId::ArtistName]))
                .await?;
            println!(
                "Current Track: {} - {}",
                attributes
                    .get(&MediaAttributeId::ArtistName)
                    .map_or("", String::as_str),
                attributes
                    .get(&MediaAttributeId::Title)
                    .map_or("", String::as_str)
            );
        }
        CurrentTrack::Id(id) => println!("Track ID: {:?}", id)
    }
    Ok(())
}


 */
