//! Invoke `yt-dlp` as a subprocess for metadata and downloads.

use crate::models::{Chapter, DownloadChoices, VideoInfo, VideoPick, VideoVariant};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

fn collect_chapters(info: &Value) -> Vec<Chapter> {
    let Some(arr) = info.get("chapters").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for c in arr {
        let start_time = c.get("start_time").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let title = c
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(Chapter {
            title,
            start_time,
        });
    }
    out.sort_by(|a, b| {
        a.start_time
            .partial_cmp(&b.start_time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
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
    let chapters = collect_chapters(&info);

    Ok(VideoInfo {
        url: url.to_string(),
        title,
        thumbnail,
        variants,
        subtitle_langs,
        chapters,
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

fn ffmpeg_bin() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

fn ffprobe_bin() -> &'static str {
    if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    }
}

fn merge_cuts(mut cuts: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    cuts.retain(|(s, e)| e > s && s.is_finite() && e.is_finite());
    if cuts.is_empty() {
        return Vec::new();
    }
    cuts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = vec![cuts[0]];
    for (s, e) in cuts.into_iter().skip(1) {
        let last = out.last_mut().expect("non-empty");
        if s <= last.1 {
            last.1 = last.1.max(e);
        } else {
            out.push((s, e));
        }
    }
    out
}

fn removed_before(merged: &[(f64, f64)], t: f64) -> f64 {
    merged
        .iter()
        .map(|&(s, e)| {
            if t <= s {
                0.0
            } else {
                (e.min(t) - s).max(0.0)
            }
        })
        .sum()
}

fn adjust_time(merged: &[(f64, f64)], t: f64) -> f64 {
    t - removed_before(merged, t)
}

fn start_inside_cut(t: f64, merged: &[(f64, f64)]) -> bool {
    merged.iter().any(|&(s, e)| t >= s && t < e)
}

fn keep_intervals(duration: f64, merged: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let duration = duration.max(0.0);
    let mut keeps = Vec::new();
    let mut cursor = 0.0_f64;
    for &(s, e) in merged {
        let s = s.clamp(0.0, duration);
        let e = e.clamp(0.0, duration);
        if cursor + 1e-6 < s {
            keeps.push((cursor, s));
        }
        cursor = cursor.max(e);
        if cursor >= duration - 1e-9 {
            break;
        }
    }
    if cursor + 1e-6 < duration {
        keeps.push((cursor, duration));
    }
    keeps.retain(|(a, b)| b - a > 1e-4);
    keeps
}

async fn probe_duration(path: &Path) -> Result<f64> {
    let output = Command::new(ffprobe_bin())
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .await
        .with_context(|| format!("failed to run `{}` — is it on your PATH?", ffprobe_bin()))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("ffprobe failed: {err}"));
    }
    let s = String::from_utf8_lossy(&output.stdout);
    let trimmed = s.trim();
    trimmed
        .parse::<f64>()
        .with_context(|| format!("invalid duration from ffprobe: {trimmed:?}"))
}

fn concat_list_line(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\'', "'\\''");
    format!("file '{s}'")
}

fn escape_ffmeta_value(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\\' => "\\\\".to_string(),
            '#' => "\\#".to_string(),
            ';' => "\\;".to_string(),
            '=' => "\\=".to_string(),
            '[' => "\\[".to_string(),
            ']' => "\\]".to_string(),
            '\n' | '\r' => " ".to_string(),
            _ => c.to_string(),
        })
        .collect()
}

fn build_ffmetadata(chapters: &[(String, f64, f64)]) -> String {
    let mut w = String::from(";FFMETADATA1\n");
    for (title, start, end) in chapters {
        let title_esc = escape_ffmeta_value(title);
        let start_us = (*start * 1_000_000.0).round() as i64;
        let end_us = (*end * 1_000_000.0).round() as i64;
        w.push_str("[CHAPTER]\n");
        w.push_str("TIMEBASE=1/1000000\n");
        w.push_str(&format!("START={start_us}\n"));
        w.push_str(&format!("END={end_us}\n"));
        w.push_str(&format!("title={title_esc}\n"));
    }
    w
}

/// Remove time ranges from `input` in-place using ffmpeg concat (`-c copy`).
/// When `chapters` is non-empty, writes adjusted chapter metadata.
pub async fn cut_segments(input: &Path, cuts: &[(f64, f64)], chapters: &[Chapter]) -> Result<()> {
    if cuts.is_empty() {
        return Ok(());
    }
    if !input.is_file() {
        return Err(anyhow!("not a file: {:?}", input));
    }

    let merged = merge_cuts(cuts.to_vec());
    if merged.is_empty() {
        return Ok(());
    }

    let duration = probe_duration(input).await?;
    let keeps = keep_intervals(duration, &merged);
    if keeps.is_empty() {
        return Err(anyhow!("cut segments cover the entire video; nothing left to save"));
    }

    let parent = input.parent().unwrap_or_else(|| Path::new("."));

    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ytdlp-tui");
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mkv");

    let mut part_paths = Vec::new();
    for (i, &(a, b)) in keeps.iter().enumerate() {
        let dur = b - a;
        if dur <= 1e-6 {
            continue;
        }
        let part = parent.join(format!("{stem}.ytdlp-tui-part{i}.{ext}"));
        let status = Command::new(ffmpeg_bin())
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-ss")
            .arg(format!("{a}"))
            .arg("-i")
            .arg(input)
            .arg("-t")
            .arg(format!("{dur}"))
            .arg("-c")
            .arg("copy")
            .arg("-avoid_negative_ts")
            .arg("make_zero")
            .arg(&part)
            .status()
            .await
            .with_context(|| format!("failed to run `{}`", ffmpeg_bin()))?;
        if !status.success() {
            let _ = std::fs::remove_file(&part);
            return Err(anyhow!("ffmpeg failed extracting segment {i}"));
        }
        part_paths.push(part);
    }

    if part_paths.is_empty() {
        return Err(anyhow!("no segments produced for concat"));
    }

    let list_path = parent.join(format!("{stem}.ytdlp-tui-concat.txt"));
    let list_body = part_paths
        .iter()
        .map(|p| concat_list_line(p.as_path()))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&list_path, format!("{list_body}\n"))
        .with_context(|| format!("write concat list {:?}", list_path))?;

    let new_duration = adjust_time(&merged, duration);
    let adjusted_chapters: Vec<(String, f64, f64)> = if chapters.is_empty() {
        Vec::new()
    } else {
        let mut adjusted_starts: Vec<f64> = Vec::new();
        let mut titles: Vec<String> = Vec::new();
        for ch in chapters {
            if start_inside_cut(ch.start_time, &merged) {
                continue;
            }
            let ns = adjust_time(&merged, ch.start_time);
            adjusted_starts.push(ns);
            titles.push(ch.title.clone());
        }
        let mut out: Vec<(String, f64, f64)> = Vec::new();
        for i in 0..adjusted_starts.len() {
            let start = adjusted_starts[i];
            let end = if i + 1 < adjusted_starts.len() {
                adjusted_starts[i + 1]
            } else {
                new_duration
            };
            if end > start + 1e-6 {
                out.push((titles[i].clone(), start, end));
            }
        }
        out
    };

    let tmp_out = parent.join(format!("{stem}.ytdlp-tui-cut-out.{ext}"));
    let meta_path = parent.join(format!("{stem}.ytdlp-tui-chapters.ffmeta"));

    let wrote_meta = if adjusted_chapters.is_empty() {
        false
    } else {
        let meta_body = build_ffmetadata(&adjusted_chapters);
        std::fs::write(&meta_path, meta_body)
            .with_context(|| format!("write ffmetadata {:?}", meta_path))?;
        true
    };

    let success = if !wrote_meta {
        Command::new(ffmpeg_bin())
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-f")
            .arg("concat")
            .arg("-safe")
            .arg("0")
            .arg("-i")
            .arg(&list_path)
            .arg("-map")
            .arg("0")
            .arg("-c")
            .arg("copy")
            .arg(&tmp_out)
            .status()
            .await
            .with_context(|| format!("failed to run `{}`", ffmpeg_bin()))?
            .success()
    } else {
        Command::new(ffmpeg_bin())
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-f")
            .arg("concat")
            .arg("-safe")
            .arg("0")
            .arg("-i")
            .arg(&list_path)
            .arg("-f")
            .arg("ffmetadata")
            .arg("-i")
            .arg(&meta_path)
            .arg("-map_metadata")
            .arg("1")
            .arg("-map_chapters")
            .arg("1")
            .arg("-map")
            .arg("0")
            .arg("-c")
            .arg("copy")
            .arg(&tmp_out)
            .status()
            .await
            .with_context(|| format!("failed to run `{}`", ffmpeg_bin()))?
            .success()
    };

    for p in &part_paths {
        let _ = std::fs::remove_file(p);
    }
    let _ = std::fs::remove_file(&list_path);
    if wrote_meta {
        let _ = std::fs::remove_file(&meta_path);
    }

    if !success {
        let _ = std::fs::remove_file(&tmp_out);
        return Err(anyhow!("ffmpeg concat or metadata merge failed"));
    }

    let backup = parent.join(format!("{stem}.ytdlp-tui-before-cut.{ext}"));
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(input, &backup).with_context(|| "backup original before replace")?;
    std::fs::rename(&tmp_out, input).with_context(|| "replace with cut file")?;
    std::fs::remove_file(&backup).ok();

    Ok(())
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
            chapters: vec![],
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
            chapters: vec![],
            cut_segments: vec![],
        };
        let args = download_args(&video, &choices).expect("args");
        let f_pos = args.iter().position(|a| a == "-f").unwrap();
        assert_eq!(args.get(f_pos + 1), Some(&"401+bestaudio/best".to_string()));
    }
}
