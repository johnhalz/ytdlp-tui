//! Domain types for metadata and user download selections.

use std::path::PathBuf;

/// One selectable video tier: height, frame rate, dynamic range, and yt-dlp `format_id` for the video stream.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoVariant {
    pub height: u32,
    pub fps: Option<f64>,
    /// From yt-dlp (`dynamic_range` / `video_dynamic_range`); `"Unknown"` if missing.
    pub dynamic_range: String,
    pub video_format_id: String,
}

impl VideoVariant {
    /// TUI label: `{h}p · {fps}fps · {DR}` with segments omitted when `fps` is missing or DR is `Unknown`.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = vec![format!("{}p", self.height)];
        if let Some(fps) = self.fps {
            let fps_str = if (fps - fps.round()).abs() < 1e-6 {
                format!("{:.0}", fps)
            } else {
                format!("{:.2}", fps)
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string()
            };
            parts.push(format!("{fps_str}fps"));
        }
        if self.dynamic_range != "Unknown" {
            parts.push(self.dynamic_range.clone());
        }
        parts.join(" · ")
    }
}

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub url: String,
    pub title: String,
    pub thumbnail: Option<String>,
    /// Best-first distinct video variants (height · fps · dynamic range).
    pub variants: Vec<VideoVariant>,
    pub subtitle_langs: Vec<String>,
}

/// How to pick the video stream for a merged download.
#[derive(Debug, Clone)]
pub enum VideoPick {
    /// `bestvideo+bestaudio/best`
    Best,
    /// Video-only format id: `{id}+bestaudio/best`
    ByFormatId {
        video_format_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct DownloadChoices {
    pub output_dir: PathBuf,
    pub video_pick: VideoPick,
    pub merge_format: String,
    pub audio_only: bool,
    pub audio_format: String,
    pub subtitle_langs: Vec<String>,
    pub embed_chapters: bool,
}

pub const MERGE_FORMATS: &[&str] = &["mp4", "mkv", "webm"];
pub const AUDIO_FORMATS: &[&str] = &["mp3", "aac", "opus", "m4a"];

#[cfg(test)]
mod tests {
    use super::VideoVariant;

    #[test]
    fn variant_label_integer_fps_and_omits_unknown_dr() {
        let v = VideoVariant {
            height: 2160,
            fps: Some(60.0),
            dynamic_range: "HDR10".to_string(),
            video_format_id: "99".to_string(),
        };
        assert_eq!(v.label(), "2160p · 60fps · HDR10");
    }

    #[test]
    fn variant_label_omits_fps_when_none() {
        let v = VideoVariant {
            height: 1080,
            fps: None,
            dynamic_range: "SDR".to_string(),
            video_format_id: "1".to_string(),
        };
        assert_eq!(v.label(), "1080p · SDR");
    }

    #[test]
    fn variant_label_omits_unknown_dr() {
        let v = VideoVariant {
            height: 720,
            fps: Some(30.0),
            dynamic_range: "Unknown".to_string(),
            video_format_id: "2".to_string(),
        };
        assert_eq!(v.label(), "720p · 30fps");
    }
}
