"""Build yt-dlp options and run downloads with progress callbacks."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from yt_dlp import YoutubeDL

from ytdlp_tui.fetcher import VideoInfo

ProgressCallback = Callable[[str], None]


@dataclass(frozen=True, slots=True)
class DownloadChoices:
    """User selections from the selector screen."""

    output_dir: Path
    height_cap: int | None
    merge_format: str
    audio_only: bool
    audio_format: str
    subtitle_langs: tuple[str, ...]


def _postprocess_audio(ext: str) -> dict:
    fmt = ext.lower().lstrip(".")
    return {
        "key": "FFmpegExtractAudio",
        "preferredcodec": fmt,
        "preferredquality": "192",
    }


def build_ydl_opts(
    video: VideoInfo,
    choices: DownloadChoices,
    progress: ProgressCallback,
) -> dict:
    outtmpl = str(choices.output_dir / "%(title)s.%(ext)s")
    opts: dict = {
        "outtmpl": outtmpl,
        "quiet": True,
        "no_warnings": True,
        "noplaylist": True,
        "progress_hooks": [lambda d: _on_progress(d, progress)],
    }

    if choices.audio_only:
        opts["format"] = "bestaudio/best"
        opts["postprocessors"] = [_postprocess_audio(choices.audio_format)]
    else:
        merge = choices.merge_format.lower()
        opts["merge_output_format"] = merge
        if choices.height_cap is None:
            opts["format"] = "bestvideo+bestaudio/best"
        else:
            h = choices.height_cap
            opts["format"] = (
                f"bestvideo[height<={h}]+bestaudio/best[height<={h}]/best"
            )

    if choices.subtitle_langs:
        opts["writesubtitles"] = True
        opts["writeautomaticsub"] = True
        opts["subtitleslangs"] = list(choices.subtitle_langs)
        opts["subtitlesformat"] = "best"
    else:
        opts["writesubtitles"] = False
        opts["writeautomaticsub"] = False

    return opts


def _on_progress(d: dict, progress: ProgressCallback) -> None:
    status = d.get("status")
    if status == "downloading":
        total = d.get("total_bytes") or d.get("total_bytes_estimate")
        done = d.get("downloaded_bytes") or 0
        if total:
            pct = min(100.0, 100.0 * float(done) / float(total))
            progress(f"Downloading… {pct:.1f}%")
        else:
            progress("Downloading…")
    elif status == "finished":
        progress("Processing merge / post-process…")


def download(video: VideoInfo, choices: DownloadChoices, progress: ProgressCallback) -> None:
    opts = build_ydl_opts(video, choices, progress)
    with YoutubeDL(opts) as ydl:
        err = ydl.download([video.url])
    if err != 0:
        msg = "Download finished with errors."
        raise RuntimeError(msg)
