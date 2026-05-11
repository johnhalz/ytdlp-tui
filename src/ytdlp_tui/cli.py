"""Typer CLI entry."""

from __future__ import annotations

from pathlib import Path

import typer

from ytdlp_tui.app import run_tui

app = typer.Typer(
    add_completion=False,
    context_settings={"help_option_names": ["-h", "--help"]},
    help="Interactive terminal UI for yt-dlp.",
)


def main() -> None:
    """Console script entry: parse argv and launch the TUI."""
    typer.run(_main_impl)


def _main_impl(
    url: str = typer.Argument(
        ...,
        metavar="URL",
        help="Video URL to download (passed to yt-dlp).",
    ),
    output_dir: Path | None = typer.Option(
        None,
        "--output-dir",
        "-o",
        help="Directory for downloaded files (default: current directory).",
    ),
) -> None:
    out = output_dir.resolve() if output_dir is not None else None
    run_tui(url, output_dir=out)


if __name__ == "__main__":
    main()