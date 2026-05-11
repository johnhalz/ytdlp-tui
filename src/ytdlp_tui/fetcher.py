"""Extract video metadata with yt-dlp."""

from __future__ import annotations

from dataclasses import dataclass

from yt_dlp import YoutubeDL


@dataclass(frozen=True, slots=True)
class VideoInfo:
    """Metadata returned from a single-URL extract."""

    url: str
    title: str
    thumbnail: str | None
    heights: tuple[int, ...]
    subtitle_langs: tuple[str, ...]


def _collect_heights(formats: list[dict] | None) -> list[int]:
    if not formats:
        return []
    heights: set[int] = set()
    for f in formats:
        height = f.get("height")
        if height is None:
            continue
        vcodec = f.get("vcodec")
        if vcodec is None or vcodec == "none":
            continue
        if isinstance(height, int):
            heights.add(height)
    return sorted(heights, reverse=True)


def _collect_subtitle_langs(info: dict) -> list[str]:
    manual = info.get("subtitles") or {}
    auto = info.get("automatic_captions") or {}
    langs = set(manual.keys()) | set(auto.keys())
    return sorted(langs, key=str.lower)


def fetch_video_info(url: str) -> VideoInfo:
    opts: dict = {
        "quiet": True,
        "no_warnings": True,
        "noplaylist": True,
        "extract_flat": False,
    }
    with YoutubeDL(opts) as ydl:
        info = ydl.extract_info(url, download=False)
    if info is None:
        msg = "No metadata returned for URL."
        raise RuntimeError(msg)

    title = str(info.get("title") or "Unknown title")
    thumb = info.get("thumbnail")
    if thumb is not None:
        thumb = str(thumb)

    formats = info.get("formats")
    if not isinstance(formats, list):
        formats_list: list[dict] = []
    else:
        formats_list = [f for f in formats if isinstance(f, dict)]

    heights = tuple(_collect_heights(formats_list))
    subtitle_langs = tuple(_collect_subtitle_langs(info))

    return VideoInfo(
        url=url,
        title=title,
        thumbnail=thumb,
        heights=heights,
        subtitle_langs=subtitle_langs,
    )
