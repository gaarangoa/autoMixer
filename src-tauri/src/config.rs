use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub data_dir: PathBuf,
    pub ollama_base_url: String,
    pub ollama_model: String,
    /// Endpoint + model for the video/vision VLM that the video-edit skill calls.
    /// Kept separate from the chat agent's model (which lives in Hermes' config).
    #[serde(default = "default_video_base_url")]
    pub video_base_url: String,
    #[serde(default = "default_video_model")]
    pub video_model: String,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub web: WebConfig,
}

fn default_video_base_url() -> String {
    "http://127.0.0.1:2256".to_string()
}

fn default_video_model() -> String {
    "qwen3.6-35b-a3b".to_string()
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
    pub fn settings_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("automixer")
            .join("settings.json")
    }

    pub fn load() -> Self {
        let settings_path = Self::settings_path();
        let default = Self::default();

        if let Ok(raw) = fs::read_to_string(&settings_path) {
            if let Ok(config) = serde_json::from_str::<Config>(&raw) {
                return config;
            }
        }

        let _ = default.save();
        default
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::settings_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(
            &path,
            serde_json::to_string_pretty(self).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("automixer"),
            ollama_base_url: "http://127.0.0.1:2256".to_string(),
            ollama_model: "qwen3.6-35b-a3b".to_string(),
            video_base_url: default_video_base_url(),
            video_model: default_video_model(),
            audio: AudioConfig::default(),
            web: WebConfig::default(),
        }
    }
}
