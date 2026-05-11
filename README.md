# ytdlp-tui

Interactive terminal UI for [yt-dlp](https://github.com/yt-dlp/yt-dlp). Pass a URL, choose resolution, container format, audio-only mode, and subtitles, then download with a simple progress view.

## Requirements

- Python 3.11+
- [FFmpeg](https://ffmpeg.org/) on your `PATH` when merging video+audio or converting audio (same as plain yt-dlp).

## Install (PyPI / uv / pip)

```bash
pip install ytdlp-tui
# or
uv tool install ytdlp-tui
```

From a checkout:

```bash
uv sync
uv run ytdlp-tui "https://www.youtube.com/watch?v=..."
```

## Usage

```text
ytdlp-tui "https://www.youtube.com/watch?v=..."
ytdlp-tui -o ~/Downloads "https://..."
```

- **Resolution**: limits combined video+audio selection (or “Best available”).
- **Container**: `merge_output_format` for the final mux (`mp4`, `mkv`, `webm`).
- **Audio only**: extracts audio and converts via yt-dlp’s FFmpeg audio postprocessor.
- **Subtitles**: multi-select from manual + automatic caption languages yt-dlp reports.

## Binary releases (GitHub)

Tagged releases attach standalone binaries built with PyInstaller (no Python needed). See [Releases](https://github.com/johnhalz/ytdlp-tui/releases).

### Homebrew (separate tap)

Homebrew’s `brew tap johnhalz/ytdlp-tui` expects a GitHub repo named **`homebrew-ytdlp-tui`** under your user (`https://github.com/johnhalz/homebrew-ytdlp-tui`). That repository is **not** created automatically; until it exists, `brew tap` will fail with “Repository not found”.

1. Create **`johnhalz/homebrew-ytdlp-tui`** (public is fine).
2. Add `Formula/ytdlp-tui.rb` using [`docs/homebrew-ytdlp-tui.rb`](docs/homebrew-ytdlp-tui.rb), filling in `version` and each `sha256` from the matching files on the [Releases](https://github.com/johnhalz/ytdlp-tui/releases) page.
3. Then:

```bash
brew tap johnhalz/ytdlp-tui
brew install ytdlp-tui
```

Until you maintain that tap, use **pip / uv** or download the **macOS arm64** binary from Releases.

### Windows (Chocolatey)

The `chocolatey/` folder contains a template package. Set `$version` and `$checksum` in `tools/chocolateyInstall.ps1`, build the `.nupkg`, and push to the Chocolatey Community Repository (or host internally). End users:

```powershell
choco install ytdlp-tui
```

## Packaging maintainers

- **GitHub release:** push a tag `v*`; the workflow uploads `ytdlp-tui`/`.exe` plus `.sha256` files (filenames match `artifact_name` in the workflow). macOS builds are **arm64 only** (Apple Silicon).
- **Homebrew:** copy [`docs/homebrew-ytdlp-tui.rb`](docs/homebrew-ytdlp-tui.rb) into a tap; set version and SHA256s from the release assets.
- **Chocolatey:** edit `chocolatey/tools/chocolateyInstall.ps1` (version, checksum, URL), then `cd chocolatey && choco pack ytdlp-tui.nuspec` and `choco push`.

## Development

```bash
uv sync --extra dev
uv run pytest  # if tests are added
uv run pyinstaller ytdlp_tui.spec
```

## License

MIT
