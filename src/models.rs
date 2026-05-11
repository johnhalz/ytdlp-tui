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

/// Chapter marker from yt-dlp metadata (`--embed-chapters` source).
#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: String,
    pub start_time: f64,
}

/// One selectable dubbed / alternate audio stream from yt-dlp `formats` (audio-only row).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTrack {
    /// BCP-47-ish language tag from yt-dlp (e.g. `en`, `fr`, `pt-BR`).
    pub language: String,
    pub format_id: String,
}

impl AudioTrack {
    /// Short label for the TUI list: `{code} ({English name})` when known, else `{code}`.
    pub fn label(&self) -> String {
        let name = language_display_name(&self.language);
        match name {
            Some(n) => format!("{} ({n})", self.language),
            None => self.language.clone(),
        }
    }
}

fn language_display_name(code: &str) -> Option<&'static str> {
    let lower = code.to_ascii_lowercase();
    match lower.as_str() {
        "ar" => Some("Arabic"),
        "bn" => Some("Bengali"),
        "cs" => Some("Czech"),
        "da" => Some("Danish"),
        "de" => Some("German"),
        "el" => Some("Greek"),
        "en" | "en-us" | "en-gb" => Some("English"),
        "es" | "es-419" => Some("Spanish"),
        "fi" => Some("Finnish"),
        "fr" => Some("French"),
        "hi" => Some("Hindi"),
        "hu" => Some("Hungarian"),
        "id" => Some("Indonesian"),
        "it" => Some("Italian"),
        "ja" => Some("Japanese"),
        "ko" => Some("Korean"),
        "ms" => Some("Malay"),
        "nl" => Some("Dutch"),
        "no" => Some("Norwegian"),
        "pl" => Some("Polish"),
        "pt" | "pt-br" | "pt_br" => Some("Portuguese"),
        "ro" => Some("Romanian"),
        "ru" => Some("Russian"),
        "sv" => Some("Swedish"),
        "ta" => Some("Tamil"),
        "te" => Some("Telugu"),
        "th" => Some("Thai"),
        "tr" => Some("Turkish"),
        "uk" => Some("Ukrainian"),
        "vi" => Some("Vietnamese"),
        "zh" | "zh-cn" | "zh-tw" => Some("Chinese"),
        _ => None,
    }
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
    /// Distinct audio-only formats by language (best-quality pick per language).
    pub audio_tracks: Vec<AudioTrack>,
    pub subtitle_langs: Vec<String>,
    pub chapters: Vec<Chapter>,
}

/// How to pick the video stream for a merged download.
#[derive(Debug, Clone)]
pub enum VideoPick {
    /// `bestvideo+bestaudio/best`
    Best,
    /// Video-only format id; paired with chosen audio in `-f` (see `DownloadChoices::audio_track`).
    ByFormatId {
        video_format_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct DownloadChoices {
    pub output_dir: PathBuf,
    pub video_pick: VideoPick,
    pub merge_format: String,
    /// `None` uses yt-dlp default merged audio (`bestaudio`); `Some(id)` picks that audio format id.
    pub audio_track: Option<String>,
    pub audio_only: bool,
    pub audio_format: String,
    pub subtitle_langs: Vec<String>,
    pub embed_chapters: bool,
    /// Original chapter times from metadata; used when rewriting chapters after sponsor cuts.
    pub chapters: Vec<Chapter>,
    /// Time ranges (seconds) to remove from the downloaded file(s).
    pub cut_segments: Vec<(f64, f64)>,
}

pub const MERGE_FORMATS: &[&str] = &["mp4", "mkv", "webm"];
pub const AUDIO_FORMATS: &[&str] = &["mp3", "aac", "opus", "m4a"];

#[cfg(test)]
mod tests {
    use super::{AudioTrack, VideoVariant};

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

    #[test]
    fn audio_track_label_known_language() {
        let t = AudioTrack {
            language: "fr".into(),
            format_id: "140".into(),
        };
        assert_eq!(t.label(), "fr (French)");
    }

    #[test]
    fn audio_track_label_unknown_language() {
        let t = AudioTrack {
            language: "zz".into(),
            format_id: "141".into(),
        };
        assert_eq!(t.label(), "zz");
    }
}
