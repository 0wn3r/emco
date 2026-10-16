//! Settings kept as JSON in the user config directory.

use std::path::PathBuf;

use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::link::PortConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub port: PortConfig,
    pub ctrl_z: bool,
    pub several: bool,
    pub idle_secs: u32,
    pub last_folder: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { port: PortConfig::default(), ctrl_z: false, several: false, idle_secs: 5, last_folder: None }
    }
}

fn path() -> PathBuf {
    glib::user_config_dir().join("tm02link").join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, json);
        }
    }
}
