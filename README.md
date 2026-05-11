# ytdlp-tui

Interactive terminal UI for [yt-dlp](https://github.com/yt-dlp/yt-dlp). Pass a URL, choose resolution, container format, audio-only mode, and subtitles, then download with a simple progress view.

This project is a **Rust** binary. It shells out to the **`yt-dlp`** executable on your `PATH` (same as using yt-dlp from the CLI).

## Requirements

- **Rust** toolchain (for `cargo install`)
- **`yt-dlp`** on your `PATH`
- **FFmpeg** on your `PATH` when merging video+audio or converting audio (same as plain yt-dlp)

## Install

```bash
cargo install ytdlp-tui
```

## Usage

```bash
ytdlp-tui "https://www.youtube.com/watch?v=..."
ytdlp-tui --output-dir ~/Downloads "https://..."
```

### Controls (selector screen)

- **Tab** / **Shift+Tab** — cycle focus (resolution, container, audio-only, audio format, subtitles, actions)
- **↑** / **↓** — change the focused option
- **Space** — toggle **Audio only** or the focused subtitle language
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
