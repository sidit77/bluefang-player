use std::fs::File;
use std::path::PathBuf;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::PROJECT_DIRS;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub bluetooth_adapter: Option<(u16, u16)>,
    pub volume_multiplier: f32,
    pub hci_dump_enabled: bool,
    pub log_level: String
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            bluetooth_adapter: None,
            volume_multiplier: 1.0,
            hci_dump_enabled: false,
            log_level: "info".to_string()
        }
    }
}

static SETTINGS_PATH: Lazy<PathBuf> = Lazy::new(|| PROJECT_DIRS.data_dir().join("settings.ron"));

impl AppSettings {
    pub fn load() -> Self {
        if SETTINGS_PATH.exists() {
            let file = File::open(&*SETTINGS_PATH).expect("Failed to open settings file");
            ron::de::from_reader(file).expect("Failed to deserialize settings")
        } else {
            std::fs::create_dir_all(SETTINGS_PATH.parent().unwrap()).expect("Failed to create settings directory");
            std::fs::write(
                &*SETTINGS_PATH,
                ron::ser::to_string_pretty(&Self::default(), Default::default()).expect("Failed to serialize default settings")
            )
            .expect("Failed to write default settings");
            Self::default()
        }
    }
}
