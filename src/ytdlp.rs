//! Invoke `yt-dlp` as a subprocess for metadata and downloads.

use crate::models::{DownloadChoices, VideoInfo};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

fn yt_dlp_bin() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

#[derive(Debug, Deserialize)]
struct DumpFormat {
    height: Option<u64>,
    vcodec: Option<String>,
}

fn collect_heights(formats: &[Value]) -> Vec<u32> {
    let mut heights: HashSet<u32> = HashSet::new();
    for f in formats {
        let Ok(fmt) = serde_json::from_value::<DumpFormat>(f.clone()) else {
            continue;
        };
        let Some(h) = fmt.height else { continue };
        let Some(vc) = fmt.vcodec.as_deref() else { continue };
        if vc == "none" {
            continue;
        }
        heights.insert(h as u32);
    }
    let mut v: Vec<u32> = heights.into_iter().collect();
    v.sort_by(|a, b| b.cmp(a));
    v
}

fn collect_subtitle_langs(info: &Value) -> Vec<String> {
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
            return Err(anyhow!("yt-dlp exited with status {:?}", output.status.code()));
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

    let heights = collect_heights(&formats);
    let subtitle_langs = collect_subtitle_langs(&info);

    Ok(VideoInfo {
        url: url.to_string(),
        title,
        thumbnail,
        heights,
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
        let fmt = match choices.height_cap {
            None => "bestvideo+bestaudio/best".to_string(),
            Some(h) => format!(
                "bestvideo[height<={h}]+bestaudio/best[height<={h}]/best"
            ),
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
) -> Result<()> {
    let args = download_args(video, choices)?;
    let mut child = Command::new(yt_dlp_bin())
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `{}`", yt_dlp_bin()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("no stderr"))?;

    let progress_out = progress.clone();
    let stdout_task = async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Some(line) = reader.next_line().await? {
            if let Some(pct) = parse_progress_line(&line) {
                let _ = progress_out.send(format!("Downloading… {pct:.1}%"));
            } else if line.contains("[download]") {
                let _ = progress_out.send("Downloading…".to_string());
            }
        }
        Ok::<(), std::io::Error>(())
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
    stdout_res.context("read yt-dlp stdout")?;
    let err_text = stderr_res.context("read yt-dlp stderr")?;

    let status = child
        .wait()
        .await
        .context("wait for yt-dlp")?;

    let stderr_joined = err_text.join("\n");

    if !status.success() {
        if stderr_joined.is_empty() {
            return Err(anyhow!(
                "yt-dlp failed with status {:?}",
                status.code()
            ));
        }
        return Err(anyhow!("{stderr_joined}"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_progress_line;

    #[test]
    fn parses_percent() {
        assert_eq!(
            parse_progress_line("[download]  45.5% of   12.00MiB at   Unknown B/s ETA Unknown"),
            Some(45.5)
        );
        assert_eq!(parse_progress_line("[download] 100% of 1MiB"), Some(100.0));
        assert_eq!(parse_progress_line("not progress"), None);
    }
}

