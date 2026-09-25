//! Invoke `yt-dlp` as a subprocess for metadata and downloads.

use crate::models::{AudioTrack, Chapter, DownloadChoices, VideoInfo, VideoPick, VideoVariant};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Printed to stdout by `--print` after each file is moved; parsed in `run_download`.
const PRINT_FILEPATH_PREFIX: &str = "ytdlp-tui-out:";
/// Printed to stdout by `--print` once downloading ends and post-processing (merge/cut/embed) starts.
const PRINT_POSTPROCESS_MARKER: &str = "ytdlp-tui-phase:post";

/// Sort used for mp4 output: same as yt-dlp's `-t mp4` preset (H.264 + AAC plays everywhere, incl. QuickTime).
const MP4_FORMAT_SORT: &str = "vcodec:h264,lang,quality,res,fps,hdr:12,acodec:aac";

/// Progress events sent from `run_download` to the UI.
#[derive(Debug)]
pub enum DlEvent {
    Progress(f64),
    PostProcessing,
}

fn yt_dlp_bin() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
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

/// Deduped video tiers (height · fps · dynamic range); `h264` set if any format in the tier is `avc1`.
///
/// Sort: **height** desc, **fps** desc (missing fps last), then **HDR-style** before **SDR** before **Unknown**.
fn collect_video_variants(formats: &[Value]) -> Vec<VideoVariant> {
    type Key = (u32, i64, String);
    let mut tiers: HashMap<Key, VideoVariant> = HashMap::new();

    for f in formats {
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
        let h264 = vc.starts_with("avc1");
        let key = (height, normalized_fps_key(fps), dynamic_range.clone());

        tiers
            .entry(key)
            .and_modify(|v| v.h264 |= h264)
            .or_insert(VideoVariant {
                height,
                fps,
                dynamic_range,
                h264,
            });
    }

    let mut variants: Vec<VideoVariant> = tiers.into_values().collect();

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
    });

    variants
}

/// Distinct languages of audio-only formats (case-insensitive), sorted.
fn collect_audio_tracks(formats: &[Value]) -> Vec<AudioTrack> {
    let mut langs: HashMap<String, String> = HashMap::new();

    for f in formats {
        let vc = f.get("vcodec").and_then(|v| v.as_str()).unwrap_or("");
        if !vc.is_empty() && vc != "none" {
            continue;
        }
        let ac = f.get("acodec").and_then(|v| v.as_str()).unwrap_or("");
        if ac.is_empty() || ac == "none" {
            continue;
        }
        let Some(lang_raw) = f.get("language").and_then(|v| v.as_str()).map(str::trim) else {
            continue;
        };
        let lang_key = lang_raw.to_ascii_lowercase();
        if lang_key.is_empty() || lang_key == "und" {
            continue;
        }
        langs.entry(lang_key).or_insert_with(|| lang_raw.to_string());
    }

    let mut tracks: Vec<(String, AudioTrack)> = langs
        .into_iter()
        .map(|(k, language)| (k, AudioTrack { language }))
        .collect();
    tracks.sort_by(|a, b| a.0.cmp(&b.0));
    tracks.into_iter().map(|(_, t)| t).collect()
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
    let mut out: Vec<Chapter> = arr
        .iter()
        .filter_map(|c| {
            Some(Chapter {
                title: c.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                start_time: c.get("start_time").and_then(|v| v.as_f64())?,
                end_time: c.get("end_time").and_then(|v| v.as_f64())?,
            })
        })
        .collect();
    out.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
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
        .kill_on_drop(true)
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

    let formats: &[Value] = info
        .get("formats")
        .and_then(|x| x.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();

    Ok(VideoInfo {
        url: url.to_string(),
        title,
        variants: collect_video_variants(formats),
        audio_tracks: collect_audio_tracks(formats),
        subtitle_langs: collect_subtitle_langs(&info),
        chapters: collect_chapters(&info),
    })
}

/// Prefer streams yt-dlp marks as original in `format_note`; fallback to plain `bestaudio`.
const BESTAUDIO_PREFER_ORIGINAL: &str = "(bestaudio[format_note*=original]/bestaudio)";

/// `-f` audio selector: chosen language, or the original track.
fn audio_selector(language: Option<&str>) -> String {
    match language {
        Some(lang) => format!("bestaudio[language=\"{lang}\"]"),
        None => BESTAUDIO_PREFER_ORIGINAL.to_string(),
    }
}

/// `-f` video selector: filter to the picked tier; the codec within it is left to `-S`.
fn video_selector(pick: &VideoPick) -> String {
    let VideoPick::Variant(v) = pick else {
        return "bestvideo".to_string();
    };
    let mut s = format!("bestvideo[height={}]", v.height);
    if let Some(fps) = v.fps {
        s.push_str(&format!("[fps={fps}]"));
    }
    if v.dynamic_range != "Unknown" {
        s.push_str(&format!("[dynamic_range=\"{}\"]", v.dynamic_range));
    }
    s
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
        "--progress-delta".into(),
        "0.25".into(),
        "--concurrent-fragments".into(),
        "4".into(),
        "-O".into(),
        format!("post_process:{PRINT_POSTPROCESS_MARKER}"),
        "-O".into(),
        format!("after_move:{PRINT_FILEPATH_PREFIX}%(filepath)s"),
        "-o".into(),
        outtmpl,
    ];

    let audio = audio_selector(choices.audio_track.as_deref());
    if choices.audio_only {
        args.push("-f".into());
        args.push(format!("{audio}/best"));
        // Prefer a source already in the target codec so extraction can skip re-encoding.
        let sort = match choices.audio_format.as_str() {
            "aac" | "m4a" => Some("acodec:aac"),
            "opus" => Some("acodec:opus"),
            _ => None,
        };
        if let Some(sort) = sort {
            args.push("-S".into());
            args.push(sort.into());
        }
        args.push("-x".into());
        args.push("--audio-format".into());
        args.push(choices.audio_format.clone());
        args.push("--audio-quality".into());
        args.push("192K".into());
    } else {
        args.push("--merge-output-format".into());
        args.push(choices.merge_format.clone());
        args.push("-f".into());
        args.push(format!("{}+{audio}/best", video_selector(&choices.video_pick)));
        let sort = match choices.merge_format.as_str() {
            "mp4" => Some(MP4_FORMAT_SORT),
            "webm" => Some("ext:webm:webm"),
            _ => None,
        };
        if let Some(sort) = sort {
            args.push("-S".into());
            args.push(sort.into());
        }

        if !choices.subtitle_langs.is_empty() {
            args.push("--write-subs".into());
            args.push("--write-auto-subs".into());
            args.push("--sub-langs".into());
            args.push(choices.subtitle_langs.join(","));
            args.push("--embed-subs".into());
        }
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
    // e.g. `[download]  12.3% of ...` or `[download] 100% of ...`
    let rest = line.strip_prefix("[download]")?.trim_start();
    let pct_part = rest.split('%').next()?.trim();
    pct_part.parse::<f64>().ok()
}

pub async fn run_download(
    video: &VideoInfo,
    choices: &DownloadChoices,
    progress: std::sync::mpsc::Sender<DlEvent>,
) -> Result<Vec<PathBuf>> {
    let args = download_args(video, choices)?;
    // ponytail: kills yt-dlp when the UI drops the task (Ctrl+C); an ffmpeg grandchild may outlive it.
    let mut child = Command::new(yt_dlp_bin())
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to spawn `{}`", yt_dlp_bin()))?;

    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

    let progress_out = progress.clone();
    let stdout_task = async move {
        let mut reader = BufReader::new(stdout).lines();
        let mut paths = Vec::<PathBuf>::new();
        while let Some(line) = reader.next_line().await? {
            let line = line.trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix(PRINT_FILEPATH_PREFIX) {
                let path = rest.trim();
                if !path.is_empty() {
                    let p = PathBuf::from(path);
                    if !paths.contains(&p) {
                        paths.push(p);
                    }
                }
            } else if line.trim() == PRINT_POSTPROCESS_MARKER {
                let _ = progress_out.send(DlEvent::PostProcessing);
            } else if let Some(pct) = parse_progress_line(line) {
                let _ = progress_out.send(DlEvent::Progress(pct));
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

    if !choices.cut_segments.is_empty() {
        let _ = progress.send(DlEvent::PostProcessing);
        // Re-time chapters only if the user wants them; otherwise the cut strips them.
        let chapters: &[Chapter] = if choices.embed_chapters { &video.chapters } else { &[] };
        for p in &paths {
            cut_segments(p, &choices.cut_segments, chapters).await?;
        }
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

/// Sort, drop invalid, and merge overlapping cut ranges.
fn merge_cuts(mut cuts: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    cuts.retain(|(s, e)| e > s && s.is_finite() && e.is_finite());
    cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (s, e) in cuts {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s.max(0.0), e)),
        }
    }
    out
}

/// Map a time on the original timeline to the cut timeline (times inside a cut land on the cut point).
fn adjust_time(merged: &[(f64, f64)], t: f64) -> f64 {
    let removed: f64 = merged
        .iter()
        .map(|&(s, e)| (e.min(t) - s).max(0.0))
        .sum();
    t - removed
}

/// ffconcat script listing `input` once per kept range (`inpoint`/`outpoint`), like yt-dlp's ModifyChapters.
/// The last range is open-ended, so the real duration isn't needed.
fn concat_script(input: &Path, merged: &[(f64, f64)]) -> String {
    let file = format!(
        "file 'file:{}'\n",
        input.to_string_lossy().replace('\'', "'\\''")
    );
    let mut w = String::from("ffconcat version 1.0\n");
    let mut inpoint: Option<f64> = None;
    for &(s, e) in merged {
        if s > 0.0 {
            w.push_str(&file);
            if let Some(i) = inpoint {
                w.push_str(&format!("inpoint {i:.6}\n"));
            }
            w.push_str(&format!("outpoint {s:.6}\n"));
        }
        inpoint = Some(e);
    }
    w.push_str(&file);
    if let Some(i) = inpoint {
        w.push_str(&format!("inpoint {i:.6}\n"));
    }
    w
}

fn escape_ffmeta_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '#' | ';' | '=' | '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// FFMETADATA chapters re-timed onto the cut timeline; chapters fully inside a cut disappear.
fn build_ffmetadata(chapters: &[Chapter], merged: &[(f64, f64)]) -> Option<String> {
    let mut w = String::from(";FFMETADATA1\n");
    let mut any = false;
    for ch in chapters {
        let start = adjust_time(merged, ch.start_time);
        let end = adjust_time(merged, ch.end_time);
        if end - start < 1e-3 {
            continue;
        }
        any = true;
        w.push_str(&format!(
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle={}\n",
            (start * 1000.0).round() as i64,
            (end * 1000.0).round() as i64,
            escape_ffmeta_value(&ch.title)
        ));
    }
    any.then_some(w)
}

/// Remove time ranges from `input` in place: one ffmpeg concat pass, stream copy (cuts snap to keyframes).
/// `chapters` are re-timed and embedded; pass `&[]` to strip chapters.
async fn cut_segments(input: &Path, cuts: &[(f64, f64)], chapters: &[Chapter]) -> Result<()> {
    let merged = merge_cuts(cuts.to_vec());
    if merged.is_empty() {
        return Ok(());
    }

    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("ytdlp-tui");
    let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("mkv");
    let list_path = parent.join(format!("{stem}.ytdlp-tui-cut.ffconcat"));
    let meta_path = parent.join(format!("{stem}.ytdlp-tui-cut.ffmeta"));
    let tmp_out = parent.join(format!("{stem}.ytdlp-tui-cut.{ext}"));

    std::fs::write(&list_path, concat_script(input, &merged))
        .with_context(|| format!("write concat list {list_path:?}"))?;
    let meta = build_ffmetadata(chapters, &merged);
    if let Some(m) = &meta {
        std::fs::write(&meta_path, m).with_context(|| format!("write ffmetadata {meta_path:?}"))?;
    }

    let mut cmd = Command::new(ffmpeg_bin());
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(["-f", "concat", "-safe", "0", "-i"])
        .arg(&list_path);
    if meta.is_some() {
        cmd.args(["-f", "ffmetadata", "-i"])
            .arg(&meta_path)
            .args(["-map_chapters", "1"]);
    } else {
        cmd.args(["-map_chapters", "-1"]);
    }
    // Same stream flags as yt-dlp: all streams, but drop the mp4 chapter-text data track.
    cmd.args(["-map", "0", "-dn", "-ignore_unknown", "-c", "copy"]);
    if matches!(ext, "mp4" | "m4a" | "mov") {
        cmd.args(["-c:s", "mov_text", "-movflags", "+faststart"]);
    }
    let output = cmd
        .arg(&tmp_out)
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("failed to run `{}` — is it on your PATH?", ffmpeg_bin()));

    let _ = std::fs::remove_file(&list_path);
    let _ = std::fs::remove_file(&meta_path);
    let output = output?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp_out);
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("ffmpeg cut failed: {}", err.trim()));
    }

    std::fs::rename(&tmp_out, input).with_context(|| format!("replace {input:?} with cut file"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn video() -> VideoInfo {
        VideoInfo {
            url: "https://example.com".into(),
            title: "t".into(),
            variants: vec![],
            audio_tracks: vec![],
            subtitle_langs: vec![],
            chapters: vec![],
        }
    }

    fn choices() -> DownloadChoices {
        DownloadChoices {
            output_dir: std::env::temp_dir(),
            video_pick: VideoPick::Best,
            merge_format: "mp4".into(),
            audio_track: None,
            audio_only: false,
            audio_format: "mp3".into(),
            subtitle_langs: vec![],
            embed_chapters: false,
            cut_segments: vec![],
        }
    }

    fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let i = args.iter().position(|a| a == flag)?;
        args.get(i + 1).map(String::as_str)
    }

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
    fn collect_variants_dedupes_and_flags_h264() {
        let formats = vec![
            json!({"format_id": "vp9", "height": 1080, "vcodec": "vp09.00.40.08", "fps": 60.0, "dynamic_range": "SDR"}),
            json!({"format_id": "avc", "height": 1080, "vcodec": "avc1.64002a", "fps": 60.0, "dynamic_range": "SDR"}),
            json!({"format_id": "4k", "height": 2160, "vcodec": "av01.0.12M.08", "fps": 60.0, "dynamic_range": "SDR"}),
        ];
        let v = collect_video_variants(&formats);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].height, 2160);
        assert!(!v[0].h264);
        assert_eq!(v[1].height, 1080);
        assert!(v[1].h264);
    }

    #[test]
    fn collect_variants_sorts_height_fps_and_hdr_before_sdr() {
        let formats = vec![
            json!({"height": 1080, "vcodec": "av01", "fps": 30.0, "dynamic_range": "SDR"}),
            json!({"height": 1080, "vcodec": "av01", "fps": 60.0, "dynamic_range": "SDR"}),
            json!({"height": 2160, "vcodec": "av01", "fps": 30.0, "dynamic_range": "HDR10"}),
            json!({"height": 1080, "vcodec": "av01", "fps": 30.0, "dynamic_range": "HDR10"}),
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
    fn collect_variants_skips_audio() {
        let formats = vec![json!({"format_id": "aud", "vcodec": "none"})];
        assert!(collect_video_variants(&formats).is_empty());
    }

    #[test]
    fn mp4_prefers_h264_aac_and_original_audio() {
        let args = download_args(&video(), &choices()).expect("args");
        assert_eq!(
            arg_after(&args, "-f"),
            Some(format!("bestvideo+{BESTAUDIO_PREFER_ORIGINAL}/best").as_str())
        );
        assert_eq!(arg_after(&args, "-S"), Some(MP4_FORMAT_SORT));
        assert_eq!(arg_after(&args, "--merge-output-format"), Some("mp4"));
    }

    #[test]
    fn mkv_has_no_sort() {
        let mut c = choices();
        c.merge_format = "mkv".into();
        let args = download_args(&video(), &c).expect("args");
        assert!(arg_after(&args, "-S").is_none());
    }

    #[test]
    fn variant_and_dub_use_filters() {
        let mut c = choices();
        c.video_pick = VideoPick::Variant(VideoVariant {
            height: 1080,
            fps: Some(60.0),
            dynamic_range: "SDR".into(),
            h264: true,
        });
        c.audio_track = Some("pt-BR".into());
        let args = download_args(&video(), &c).expect("args");
        assert_eq!(
            arg_after(&args, "-f"),
            Some(r#"bestvideo[height=1080][fps=60][dynamic_range="SDR"]+bestaudio[language="pt-BR"]/best"#)
        );
    }

    #[test]
    fn variant_omits_unknown_fields() {
        let pick = VideoPick::Variant(VideoVariant {
            height: 720,
            fps: None,
            dynamic_range: "Unknown".into(),
            h264: false,
        });
        assert_eq!(video_selector(&pick), "bestvideo[height=720]");
    }

    #[test]
    fn audio_only_args() {
        let mut c = choices();
        c.audio_only = true;
        c.audio_format = "m4a".into();
        c.subtitle_langs = vec!["en".into()];
        let args = download_args(&video(), &c).expect("args");
        assert_eq!(
            arg_after(&args, "-f"),
            Some(format!("{BESTAUDIO_PREFER_ORIGINAL}/best").as_str())
        );
        assert_eq!(arg_after(&args, "-S"), Some("acodec:aac"));
        assert!(args.iter().any(|a| a == "-x"));
        assert!(!args.iter().any(|a| a == "--embed-subs"));
    }

    #[test]
    fn subs_are_embedded() {
        let mut c = choices();
        c.subtitle_langs = vec!["en".into(), "fr".into()];
        let args = download_args(&video(), &c).expect("args");
        assert_eq!(arg_after(&args, "--sub-langs"), Some("en,fr"));
        assert!(args.iter().any(|a| a == "--embed-subs"));
    }

    #[test]
    fn merge_cuts_sorts_and_merges_overlaps() {
        let m = merge_cuts(vec![(435.185, 448.455), (204.3, 259.9), (435.0, 447.8), (5.0, 5.0)]);
        assert_eq!(m, vec![(204.3, 259.9), (435.0, 448.455)]);
    }

    #[test]
    fn concat_script_keeps_gaps_and_open_end() {
        let p = Path::new("/v/it's.mp4");
        let s = concat_script(p, &[(0.0, 10.0), (20.0, 30.0)]);
        assert_eq!(
            s,
            "ffconcat version 1.0\n\
             file 'file:/v/it'\\''s.mp4'\ninpoint 10.000000\noutpoint 20.000000\n\
             file 'file:/v/it'\\''s.mp4'\ninpoint 30.000000\n"
        );
        let s = concat_script(p, &[(20.0, 30.0)]);
        assert_eq!(
            s,
            "ffconcat version 1.0\n\
             file 'file:/v/it'\\''s.mp4'\noutpoint 20.000000\n\
             file 'file:/v/it'\\''s.mp4'\ninpoint 30.000000\n"
        );
    }

    #[test]
    fn ffmetadata_retimes_chapters() {
        let ch = |t: &str, s: f64, e: f64| Chapter { title: t.into(), start_time: s, end_time: e };
        let chapters = vec![ch("a", 0.0, 100.0), ch("b=1", 100.0, 150.0), ch("c", 150.0, 200.0)];
        // Cut 90..160: "b" vanishes, "c" starts at the cut point.
        let m = build_ffmetadata(&chapters, &[(90.0, 160.0)]).expect("chapters");
        assert!(m.contains("START=0\nEND=90000\ntitle=a\n"));
        assert!(!m.contains("title=b"));
        assert!(m.contains("START=90000\nEND=130000\ntitle=c\n"));
        assert!(build_ffmetadata(&[], &[(1.0, 2.0)]).is_none());
    }

    #[test]
    fn collect_audio_tracks_dedupes_by_language() {
        let formats = vec![
            json!({"format_id": "140", "vcodec": "none", "acodec": "aac", "language": "fr"}),
            json!({"format_id": "251", "vcodec": "none", "acodec": "opus", "language": "FR"}),
            json!({"format_id": "139", "vcodec": "none", "acodec": "aac", "language": "en"}),
        ];
        let t = collect_audio_tracks(&formats);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].language, "en");
        assert_eq!(t[1].language.to_ascii_lowercase(), "fr");
    }

    #[test]
    fn collect_audio_tracks_skips_video_and_und() {
        let formats = vec![
            json!({"format_id": "401", "vcodec": "av01", "acodec": "none", "height": 1080}),
            json!({"format_id": "140", "vcodec": "none", "acodec": "aac", "language": "und"}),
            json!({"format_id": "141", "vcodec": "none", "acodec": "none", "language": "en"}),
        ];
        assert!(collect_audio_tracks(&formats).is_empty());
    }
}
