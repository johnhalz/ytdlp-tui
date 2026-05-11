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

Create a tap repository (for example `homebrew-ytdlp-tui`) and add the formula from [`docs/homebrew-ytdlp-tui.rb`](docs/homebrew-ytdlp-tui.rb), updating version and per-arch `sha256` values to match the release assets. Then:

```bash
brew tap johnhalz/ytdlp-tui
brew install ytdlp-tui
```

### Windows (Chocolatey)

The `chocolatey/` folder contains a template package. Set `$version` and `$checksum` in `tools/chocolateyInstall.ps1`, build the `.nupkg`, and push to the Chocolatey Community Repository (or host internally). End users:

```powershell
choco install ytdlp-tui
```

## Packaging maintainers

- **GitHub release:** push a tag `v*`; the workflow uploads `ytdlp-tui`/`.exe` plus `.sha256` files (filenames match `artifact_name` in the workflow).
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
