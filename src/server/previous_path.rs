use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::extractor::extractor::DATA_DIR;

#[derive(Deserialize, Serialize, Clone)]
pub struct PreviousPage {
    pub path: String,
}

impl PreviousPage {
    pub async fn set(path: &str) -> Result<()> {
        let previous_file = PathBuf::from(DATA_DIR).join("previous.json");
        let previous = Self {
            path: path.to_string(),
        };
        let json = serde_json::to_string_pretty(&previous)?;
        fs::write(previous_file, json).await?;
        return anyhow::Ok(());
    }

    pub async fn get() -> Option<String> {
        let data = PathBuf::from(DATA_DIR).join("previous.json");
        if let Ok(prev_json) = fs::read_to_string(data).await {
            if let Ok(previous) = serde_json::from_str::<Self>(&prev_json) {
                return Some(previous.path);
            }
        }
        return None;
    }
}
