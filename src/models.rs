//! Domain types for metadata and user download selections.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub url: String,
    pub title: String,
    pub thumbnail: Option<String>,
    pub heights: Vec<u32>,
    pub subtitle_langs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadChoices {
    pub output_dir: PathBuf,
    /// `None` means best available resolution.
    pub height_cap: Option<u32>,
    pub merge_format: String,
    pub audio_only: bool,
    pub audio_format: String,
    pub subtitle_langs: Vec<String>,
}

pub const MERGE_FORMATS: &[&str] = &["mp4", "mkv", "webm"];
pub const AUDIO_FORMATS: &[&str] = &["mp3", "aac", "opus", "m4a"];
