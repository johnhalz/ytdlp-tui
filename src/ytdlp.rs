//! Invoke `yt-dlp` as a subprocess for metadata and downloads.

use crate::models::{DownloadChoices, VideoInfo, VideoPick, VideoVariant};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Printed to stdout by `--print` after each file is moved; parsed in `run_download`.
const PRINT_FILEPATH_PREFIX: &str = "ytdlp-tui-out:";

fn yt_dlp_bin() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

fn format_id_str(fmt: &Value) -> Option<String> {
    match fmt.get("format_id")? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Rounded fps for dedupe keys: two decimals (e.g. 29.97). `None` when source has no fps.
fn normalized_fps_key(fps: Option<f64>) -> i64 {
    let Some(f) = fps else {
        return i64::MIN;
    };
    (f * 100.0).round() as i64
}

fn read_dynamic_range(fmt: &Value) -> String {
    let dr = fmt
        .get("dynamic_range")
        .or_else(|| fmt.get("video_dynamic_range"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match dr {
        Some(s) => s.to_string(),
        None => "Unknown".to_string(),
    }
}

fn read_fps(fmt: &Value) -> Option<f64> {
    fmt.get("fps").and_then(|v| v.as_f64())
}

fn format_score(fmt: &Value) -> u64 {
    if let Some(tbr) = fmt.get("tbr").and_then(|v| v.as_f64()) {
        return tbr.max(0.0) as u64;
    }
    if let Some(fs) = fmt.get("filesize").and_then(|v| v.as_u64()) {
        return fs;
    }
    if let Some(fs) = fmt.get("filesize_approx").and_then(|v| v.as_u64()) {
        return fs;
    }
    0
}

/// Deduped video variants, each tied to one yt-dlp video `format_id` (best scored per bucket).
///
/// Sort: **height** desc, **fps** desc (missing fps last), then **HDR-style** before **SDR** before **Unknown**.
fn collect_video_variants(formats: &[Value]) -> Vec<VideoVariant> {
    type Key = (u32, i64, String);
    let mut best: HashMap<Key, (u64, VideoVariant)> = HashMap::new();

    for f in formats {
        let Some(fid) = format_id_str(f) else { continue };
        let Some(h) = f.get("height").and_then(|v| v.as_u64()) else {
            continue;
        };
        let vc = f.get("vcodec").and_then(|v| v.as_str()).unwrap_or("");
        if vc.is_empty() || vc == "none" {
            continue;
        }

        let height = h as u32;
        let fps = read_fps(f);
        let dynamic_range = read_dynamic_range(f);
        let key = (
            height,
            normalized_fps_key(fps),
            dynamic_range.clone(),
        );

        let score = format_score(f);
        let candidate = VideoVariant {
            height,
            fps,
            dynamic_range,
            video_format_id: fid,
        };

        best.entry(key)
            .and_modify(|e| {
                if score > e.0 {
                    *e = (score, candidate.clone());
                }
            })
            .or_insert((score, candidate));
    }

    let mut variants: Vec<VideoVariant> = best.into_values().map(|(_, v)| v).collect();

    fn dr_rank(s: &str) -> u8 {
        match s {
            "SDR" => 1,
            "Unknown" => 2,
            _ => 0,
        }
    }

    variants.sort_by(|a, b| {
        b.height
            .cmp(&a.height)
            .then_with(|| {
                let af = a.fps.unwrap_or(f64::NEG_INFINITY);
                let bf = b.fps.unwrap_or(f64::NEG_INFINITY);
                bf.partial_cmp(&af).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| dr_rank(&a.dynamic_range).cmp(&dr_rank(&b.dynamic_range)))
            .then_with(|| a.video_format_id.cmp(&b.video_format_id))
    });

    variants
}

fn collect_subtitle_langs(info: &Value) -> Vec<String> {
    use std::collections::HashSet;
    let mut langs: HashSet<String> = HashSet::new();
    if let Some(m) = info.get("subtitles").and_then(|x| x.as_object()) {
        for k in m.keys() {
            langs.insert(k.clone());
        }
    }
    if let Some(m) = info.get("automatic_captions").and_then(|x| x.as_object()) {
        for k in m.keys() {
            langs.insert(k.clone());
        }
    }
    let mut v: Vec<String> = langs.into_iter().collect();
    v.sort_by_key(|a| a.to_lowercase());
    v
}

pub async fn fetch_video_info(url: &str) -> Result<VideoInfo> {
    let output = Command::new(yt_dlp_bin())
        .args([
            "--dump-json",
            "--no-playlist",
            "--quiet",
            "--no-warnings",
            url,
        ])
        .output()
        .await
        .with_context(|| format!("failed to run `{}` — is it on your PATH?", yt_dlp_bin()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim();
        if msg.is_empty() {
            return Err(anyhow!(
                "yt-dlp exited with status {:?}",
                output.status.code()
            ));
        }
        return Err(anyhow!("{msg}"));
    }

    let info: Value = serde_json::from_slice(&output.stdout).context("invalid JSON from yt-dlp")?;

    let title = info
        .get("title")
        .and_then(|x| x.as_str())
        .unwrap_or("Unknown title")
        .to_string();

    let thumbnail = info
        .get("thumbnail")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    let formats: Vec<Value> = info
        .get("formats")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();

    let variants = collect_video_variants(&formats);
    let subtitle_langs = collect_subtitle_langs(&info);

    Ok(VideoInfo {
        url: url.to_string(),
        title,
        thumbnail,
        variants,
        subtitle_langs,
    })
}

/// Build download arguments (including the URL at the end).
fn download_args(video: &VideoInfo, choices: &DownloadChoices) -> Result<Vec<String>> {
    if !choices.output_dir.as_path().is_dir() {
        return Err(anyhow!(
            "output directory does not exist: {:?}",
            choices.output_dir
        ));
    }

    let outtmpl = choices
        .output_dir
        .join("%(title)s.%(ext)s")
        .to_string_lossy()
        .replace('\\', "/");

    let mut args: Vec<String> = vec![
        "--no-playlist".into(),
        "--quiet".into(),
        "--no-warnings".into(),
        "--newline".into(),
        "--progress".into(),
        "-O".into(),
        format!("after_move:{PRINT_FILEPATH_PREFIX}%(filepath)s"),
        "-o".into(),
        outtmpl,
    ];

    if choices.audio_only {
        args.push("-f".into());
        args.push("bestaudio/best".into());
        args.push("-x".into());
        args.push("--audio-format".into());
        args.push(choices.audio_format.clone());
        args.push("--audio-quality".into());
        args.push("192".into());
    } else {
        args.push("--merge-output-format".into());
        args.push(choices.merge_format.clone());
        let fmt = match &choices.video_pick {
            VideoPick::Best => "bestvideo+bestaudio/best".to_string(),
            VideoPick::ByFormatId { video_format_id } => {
                format!("{video_format_id}+bestaudio/best")
            }
        };
        args.push("-f".into());
        args.push(fmt);
    }

    if !choices.subtitle_langs.is_empty() {
        args.push("--write-subs".into());
        args.push("--write-auto-subs".into());
        let langs = choices.subtitle_langs.join(",");
        args.push("--sub-langs".into());
        args.push(langs);
        args.push("--sub-format".into());
        args.push("best".into());
    }

    if choices.embed_chapters {
        args.push("--embed-chapters".into());
    }

    args.push(video.url.clone());
    Ok(args)
}

/// Parse a yt-dlp `--newline` progress line; returns percent if present.
pub fn parse_progress_line(line: &str) -> Option<f64> {
    let line = line.trim();
    if !line.starts_with("[download]") {
        return None;
    }
    // e.g. `[download]  12.3% of ...` or `[download] 100% of ...`
    let rest = line.strip_prefix("[download]")?.trim_start();
    let pct_part = rest.split('%').next()?.trim();
    pct_part.parse::<f64>().ok()
}

pub async fn run_download(
    video: &VideoInfo,
    choices: &DownloadChoices,
    progress: std::sync::mpsc::Sender<String>,
) -> Result<Vec<PathBuf>> {
    let args = download_args(video, choices)?;
    let mut child = Command::new(yt_dlp_bin())
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `{}`", yt_dlp_bin()))?;

    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

    let progress_out = progress.clone();
    let prefix = PRINT_FILEPATH_PREFIX.to_string();
    let stdout_task = async move {
        let mut reader = BufReader::new(stdout).lines();
        let mut paths = Vec::<PathBuf>::new();
        while let Some(line) = reader.next_line().await? {
            let line = line.trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix(prefix.as_str()) {
                let path = rest.trim();
                if !path.is_empty() {
                    let p = PathBuf::from(path);
                    if !paths.contains(&p) {
                        paths.push(p);
                    }
                }
            } else if let Some(pct) = parse_progress_line(line) {
                let _ = progress_out.send(format!("Downloading… {pct:.1}%"));
            } else if line.contains("[download]") {
                let _ = progress_out.send("Downloading…".to_string());
            }
        }
        Ok::<Vec<PathBuf>, std::io::Error>(paths)
    };

    let stderr_task = async move {
        let mut reader = BufReader::new(stderr).lines();
        let mut err_lines = Vec::<String>::new();
        while let Some(line) = reader.next_line().await? {
            err_lines.push(line);
        }
        Ok::<Vec<String>, std::io::Error>(err_lines)
    };

    let (stdout_res, stderr_res) = tokio::join!(stdout_task, stderr_task);
    let paths = stdout_res.context("read yt-dlp stdout")?;
    let err_text = stderr_res.context("read yt-dlp stderr")?;

    let status = child.wait().await.context("wait for yt-dlp")?;

    let stderr_joined = err_text.join("\n");

    if !status.success() {
        if stderr_joined.is_empty() {
            return Err(anyhow!("yt-dlp failed with status {:?}", status.code()));
        }
        return Err(anyhow!("{stderr_joined}"));
    }

    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_percent() {
        assert_eq!(
            parse_progress_line("[download]  45.5% of   12.00MiB at   Unknown B/s ETA Unknown"),
            Some(45.5)
        );
        assert_eq!(parse_progress_line("[download] 100% of 1MiB"), Some(100.0));
        assert_eq!(parse_progress_line("not progress"), None);
    }

    #[test]
    fn collect_variants_dedupes_and_keeps_higher_tbr() {
        let formats = vec![
            json!({
                "format_id": "low",
                "height": 1080,
                "vcodec": "avc1",
                "fps": 60.0,
                "dynamic_range": "SDR",
                "tbr": 100.0,
            }),
            json!({
                "format_id": "high",
                "height": 1080,
                "vcodec": "avc1",
                "fps": 60.0,
                "dynamic_range": "SDR",
                "tbr": 5000.0,
            }),
        ];
        let v = collect_video_variants(&formats);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].video_format_id, "high");
    }

    #[test]
    fn collect_variants_sorts_height_fps_and_hdr_before_sdr() {
        let formats = vec![
            json!({
                "format_id": "a",
                "height": 1080,
                "vcodec": "av01",
                "fps": 30.0,
                "dynamic_range": "SDR",
                "tbr": 100.0,
            }),
            json!({
                "format_id": "b",
                "height": 1080,
                "vcodec": "av01",
                "fps": 60.0,
                "dynamic_range": "SDR",
                "tbr": 100.0,
            }),
            json!({
                "format_id": "c",
                "height": 2160,
                "vcodec": "av01",
                "fps": 30.0,
                "dynamic_range": "HDR10",
                "tbr": 100.0,
            }),
            json!({
                "format_id": "d",
                "height": 1080,
                "vcodec": "av01",
                "fps": 30.0,
                "dynamic_range": "HDR10",
                "tbr": 100.0,
            }),
        ];
        let v = collect_video_variants(&formats);
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].height, 2160);
        assert_eq!(v[1].height, 1080);
        assert_eq!(v[1].fps, Some(60.0));
        assert_eq!(v[2].dynamic_range, "HDR10");
        assert_eq!(v[2].height, 1080);
        assert_eq!(v[3].dynamic_range, "SDR");
    }

    #[test]
    fn collect_variants_skips_audio_and_missing_format_id() {
        let formats = vec![
            json!({
                "height": 1080,
                "vcodec": "avc1",
                "fps": 60.0,
                "dynamic_range": "SDR",
            }),
            json!({
                "format_id": "aud",
                "vcodec": "none",
            }),
        ];
        let v = collect_video_variants(&formats);
        assert!(v.is_empty());
    }

    #[test]
    fn download_args_by_format_id() {
        let video = VideoInfo {
            url: "https://example.com".into(),
            title: "t".into(),
            thumbnail: None,
            variants: vec![],
            subtitle_langs: vec![],
        };
        let choices = DownloadChoices {
            output_dir: std::env::temp_dir(),
            video_pick: VideoPick::ByFormatId {
                video_format_id: "401".into(),
            },
            merge_format: "mkv".into(),
            audio_only: false,
            audio_format: "mp3".into(),
            subtitle_langs: vec![],
            embed_chapters: false,
        };
        let args = download_args(&video, &choices).expect("args");
        let f_pos = args.iter().position(|a| a == "-f").unwrap();
        assert_eq!(args.get(f_pos + 1), Some(&"401+bestaudio/best".to_string()));
    }
}
