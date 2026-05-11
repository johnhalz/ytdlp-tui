# ytdlp-tui

Interactive terminal UI for [yt-dlp](https://github.com/yt-dlp/yt-dlp). Pass a URL, pick download options in the selector, then watch progress and get printed paths when finished.

This project is a **Rust** binary. It shells out to **`yt-dlp`** on your **`PATH`** (same idea as running yt-dlp yourself). Optional **[SponsorBlock](https://sponsor.ajay.app/)** cutting runs **`ffmpeg`** (and **`ffprobe`** for duration) after download.

## Features

These are the things users can do from the tool:

- **Selector overview** — Shows **title**, **URL**, **save directory**, and optional **thumbnail URL** from metadata before you download.
- **URL plus optional output directory** — CLI positional **`URL`** and **`--output-dir` / `-o`**; defaults to the current working directory.
- **Metadata-first workflow** — Fetches video info with yt-dlp and builds choices before any download (**single video**, **`--no-playlist`**).
- **Resolution picker** — **Best available** or a specific tier derived from yt-dlp formats (**height**, **frame rate**, **HDR / SDR** when reported).
- **Container format** — For merged video downloads: **`mp4`**, **`mkv`**, or **`webm`** (`--merge-output-format`).
- **Audio track choice** — **Original (default)** prefers streams yt-dlp marks as **original** (via `format_note`), then falls back to **`bestaudio`**. Or pick an explicit **dub / alternate language** row when multiple audio-only tracks exist (best variant per language from metadata).
- **Audio-only mode** — Download **`bestaudio`** (with the same default-original preference), extract audio (**`-x`**), choose encoder/container (**`mp3`**, **`aac`**, **`opus`**, **`m4a`**), fixed **`--audio-quality` `192`**.
- **Subtitles** — Multi-select languages discovered from **manual** and **automatic** captions (**`--write-subs`**, **`--write-auto-subs`**, **`--sub-langs`**, best subtitle format).
- **Chapters** — Toggle **`--embed-chapters`** from metadata.
- **SponsorBlock** — Loads sponsor segments for **YouTube** URLs (HTTPS API); optionally tick segments to **remove with ffmpeg** after download (**`-c copy`** concat); chapter timings can be **rewritten** to match the cut file.
- **Progress UI** — Compact inline terminal UI with a **progress bar** driven by yt-dlp **`--newline`** **`--progress`** output.
- **Saved paths** — On success, prints **`Saved:`** and canonical paths reported by yt-dlp (or the output directory if none were captured).
- **Errors in-app** — Failed metadata fetch, download, or post-processing shows a message screen instead of a silent exit.

## Requirements

- **Rust** toolchain (for `cargo install`)
- **`yt-dlp`** on your `PATH`
- **FFmpeg** and **ffprobe** on your `PATH` when merging video+audio, converting audio, or **removing selected SponsorBlock segments** after download (same merging needs as plain yt-dlp)

## Install

```bash
cargo install ytdlp-tui
```

## Updating

Rebuild and reinstall the latest crates.io release (overwrites the previous install):

```bash
cargo install ytdlp-tui --force
```

See what is installed with `cargo install --list`; check the binary with `ytdlp-tui --version`.

## Usage

```bash
ytdlp-tui "https://www.youtube.com/watch?v=..."
ytdlp-tui --output-dir ~/Downloads "https://..."
```

### Controls (selector screen)

- **Tab** / **Shift+Tab** — cycle focus (resolution, container, audio track, audio-only, audio format, subtitles, embed chapters, SponsorBlock, Download / Quit actions)
- **↑** / **↓** — change the focused option
- **Space** — toggle **Audio only**, **Embed chapters**, **SponsorBlock** cut (when that row is focused), or the focused subtitle language
- **Enter** — start download (when **Download** is focused) or quit (when **Quit** is focused)
- **q** / **Esc** — exit

## Development

```bash
cargo build
cargo test
cargo run -- "https://www.youtube.com/watch?v=..."
```

## GitHub Actions

- **CI** runs `cargo test` and `cargo build --release` on pushes and PRs to `main`.
- **Publish** runs when a **`v*`** tag is pushed (`v0.3.0`, etc.): one job creates the **GitHub Release** from that tag (with generated notes); another publishes to crates.io (requires **`CARGO_REGISTRY_TOKEN`** with a [crates.io token](https://crates.io/settings/tokens)). Tag the commit whose **`Cargo.toml`** version matches the tag (after bumping to `0.3.0`, run `git tag v0.3.0` and push the tag).

## License

MIT
