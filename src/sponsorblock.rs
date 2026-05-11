//! Fetch sponsored / skip segments from the SponsorBlock public API.

use anyhow::{Context, Result};
use serde::Deserialize;

/// One reported segment: start and end times in seconds (video timeline).
#[derive(Debug, Clone)]
pub struct SponsorSegment {
    pub start: f64,
    pub end: f64,
    pub category: String,
}

#[derive(Debug, Deserialize)]
struct ApiSegment {
    segment: Vec<f64>,
    category: String,
}

const API_BASE: &str = "https://sponsor.ajay.app/api/skipSegments";

/// Categories to request (JSON array). Matches typical SponsorBlock “skip” types.
const CATEGORIES_JSON: &str = r#"["sponsor","selfpromo","interaction","intro","outro","preview","music_offtopic","filler"]"#;

/// Extract a YouTube `videoID` from common URL shapes. Returns `None` if not recognized as YouTube.
pub fn extract_youtube_id(url: &str) -> Option<String> {
    let u = url.trim();
    let lower = u.to_lowercase();

    if let Some(idx) = lower.find("youtu.be/") {
        let after = &u[idx + "youtu.be/".len()..];
        return sanitize_id(take_id_token(after));
    }

    if !lower.contains("youtube.com") {
        return None;
    }

    if let Some(v) = query_param(u, "v") {
        return sanitize_id(v);
    }

    for needle in ["/shorts/", "/embed/", "/live/"] {
        if let Some(idx) = lower.find(needle) {
            let after = &u[idx + needle.len()..];
            return sanitize_id(take_id_token(after));
        }
    }

    None
}

fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let lower = url.to_lowercase();
    let needle = format!("{key}=").to_lowercase();
    let mut search = 0usize;
    while let Some(rel) = lower[search..].find(&needle) {
        let abs = search + rel;
        let prev_ok = if abs == 0 {
            true
        } else {
            matches!(url.as_bytes().get(abs - 1).copied(), Some(b'?' | b'&'))
        };
        if prev_ok {
            let after = abs + needle.len();
            let rest = &url[after..];
            let end = rest.find(['&', '#']).unwrap_or(rest.len());
            return Some(rest[..end].trim());
        }
        search = abs + needle.len();
    }
    None
}


fn take_id_token(s: &str) -> &str {
    s.split(&['?', '#', '/'][..]).next().unwrap_or(s)
}

fn sanitize_id(raw: &str) -> Option<String> {
    let t = take_id_token(raw);
    if t.is_empty() {
        return None;
    }
    if t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        Some(t.to_string())
    } else {
        None
    }
}

/// Fetch skip segments for `url`. Returns an empty list when there are none or the video is unknown (HTTP 404).
/// Non-YouTube URLs yield an empty list without error.
pub async fn fetch_segments(url: &str) -> Result<Vec<SponsorSegment>> {
    let Some(video_id) = extract_youtube_id(url) else {
        return Ok(Vec::new());
    };

    let client = reqwest::Client::builder()
        .user_agent(concat!("ytdlp-tui/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")?;

    let mut api_url = reqwest::Url::parse(API_BASE).context("parse SponsorBlock URL")?;
    {
        let mut pairs = api_url.query_pairs_mut();
        pairs.append_pair("videoID", &video_id);
        pairs.append_pair("categories", CATEGORIES_JSON);
    }

    let response = client.get(api_url).send().await.context("SponsorBlock request")?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("SponsorBlock HTTP {status}: {body}");
    }

    let raw: Vec<ApiSegment> = response
        .json()
        .await
        .context("parse SponsorBlock JSON")?;

    let mut out = Vec::new();
    for row in raw {
        if row.segment.len() < 2 {
            continue;
        }
        let start = row.segment[0];
        let end = row.segment[1];
        if end > start && start.is_finite() && end.is_finite() {
            out.push(SponsorSegment {
                start,
                end,
                category: row.category,
            });
        }
    }

    out.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.end.partial_cmp(&b.end).unwrap_or(std::cmp::Ordering::Equal))
    });

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_watch_v() {
        assert_eq!(
            extract_youtube_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&foo=1").as_deref(),
            Some("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn extracts_short_url() {
        assert_eq!(
            extract_youtube_id("https://youtu.be/dQw4w9WgXcQ?t=42").as_deref(),
            Some("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn extracts_shorts() {
        assert_eq!(
            extract_youtube_id("https://www.youtube.com/shorts/AbCdEfGhIjK").as_deref(),
            Some("AbCdEfGhIjK")
        );
    }

    #[test]
    fn non_youtube_none() {
        assert!(extract_youtube_id("https://example.com/video/1").is_none());
    }
}
