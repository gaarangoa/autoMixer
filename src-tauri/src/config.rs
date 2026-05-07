use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub data_dir: PathBuf,
    pub ollama_base_url: String,
    pub ollama_model: String,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub web: WebConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioConfig {
    pub block_size: usize,
    pub output_device: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebConfig {
    pub host: String,
    pub port: u16,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { block_size: 512, output_device: None }
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        Self { host: "127.0.0.1".to_string(), port: 5178 }
    }
}

impl Config {
    pub fn load() -> Self {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("automixer");
        let settings_path = config_dir.join("settings.json");
        let default = Self::default();

        if let Ok(raw) = fs::read_to_string(&settings_path) {
            if let Ok(config) = serde_json::from_str::<Config>(&raw) {
                return config;
            }
        }

        let _ = fs::create_dir_all(&config_dir);
        let _ = fs::write(
            settings_path,
            serde_json::to_string_pretty(&default).unwrap_or_else(|_| "{}".to_string()),
        );
        default
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("automixer"),
            ollama_base_url: "http://localhost:11434".to_string(),
            ollama_model: "gpt-oss:20b".to_string(),
            audio: AudioConfig::default(),
            web: WebConfig::default(),
        }
    }
}
