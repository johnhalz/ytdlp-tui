"""Textual TUI: loading, selection, download, and result screens."""

from __future__ import annotations

import threading
from pathlib import Path

from textual import on
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, ScrollableContainer, Vertical
from textual.screen import Screen
from textual.widgets import (
    Button,
    Footer,
    Header,
    Label,
    LoadingIndicator,
    ProgressBar,
    Rule,
    Select,
    SelectionList,
    Static,
    Switch,
)

from ytdlp_tui.downloader import DownloadChoices, download
from ytdlp_tui.fetcher import VideoInfo, fetch_video_info


def run_tui(url: str, output_dir: Path | None = None) -> None:
    """Run the interactive UI for a single URL."""
    out = output_dir if output_dir is not None else Path.cwd()
    YtDlpTuiApp(url, out).run()


class YtDlpTuiApp(App[None]):
    """Root application."""

    TITLE = "ytdlp-tui"

    CSS = """
    #card {
        width: min(100% - 4, 88);
        height: auto;
        max-height: 100% - 4;
        border: tall $primary;
        padding: 1 2;
        background: $surface;
    }
    #title { margin-bottom: 1; text-style: bold; }
    #meta { color: $text-muted; margin-bottom: 1; }
    .row { height: auto; margin-bottom: 1; }
    Label.option-label { margin-top: 1; text-style: bold; }
    """

    BINDINGS = [
        Binding("q", "quit", "Quit", show=True),
    ]

    def __init__(self, url: str, output_dir: Path) -> None:
        super().__init__()
        self.start_url = url
        self.output_dir = output_dir

    def on_mount(self) -> None:
        self.push_screen(LoadingScreen(self.start_url, self.output_dir))

    def action_quit(self) -> None:
        self.exit()


class LoadingScreen(Screen[None]):
    """Fetch metadata in a background thread."""

    def __init__(self, url: str, output_dir: Path) -> None:
        super().__init__()
        self.fetch_url = url
        self.output_dir = output_dir

    def compose(self) -> ComposeResult:
        yield Header()
        with Vertical(id="card"):
            yield Static(f"[b]Loading metadata[/b]\n{self.fetch_url}", id="status")
            yield LoadingIndicator()
        yield Footer()

    def on_mount(self) -> None:
        app = self.app
        url = self.fetch_url
        out = self.output_dir

        def task() -> None:
            try:
                info = fetch_video_info(url)

                def ok() -> None:
                    app.pop_screen()
                    app.push_screen(SelectorScreen(info, out))

                app.call_from_thread(ok)
            except Exception as e:
                msg = str(e)

                def err() -> None:
                    app.pop_screen()
                    app.push_screen(MessageScreen("Error", msg))

                app.call_from_thread(err)

        threading.Thread(target=task, daemon=True).start()


class SelectorScreen(Screen[None]):
    """Pick format options and start the download."""

    def __init__(self, video: VideoInfo, output_dir: Path) -> None:
        super().__init__()
        self.video = video
        self.output_dir = output_dir

    def compose(self) -> ComposeResult:
        yield Header()
        with Vertical(id="card"):
            yield Static(self._title_markup(), id="title")
            meta_parts = [f"URL: {self.video.url}", f"Save to: {self.output_dir}"]
            if self.video.thumbnail:
                meta_parts.append(f"Thumbnail: {self.video.thumbnail}")
            yield Static("\n".join(meta_parts), id="meta")
            yield Rule()
            yield Label("Resolution", classes="option-label")
            yield Select(
                self._resolution_options(),
                id="resolution",
                allow_blank=False,
            )
            yield Label("Container format", classes="option-label")
            yield Select(
                (
                    ("MP4", "mp4"),
                    ("MKV", "mkv"),
                    ("WebM", "webm"),
                ),
                id="merge",
                allow_blank=False,
            )
            yield Horizontal(
                Label("Audio only"),
                Switch(value=False, id="audio_only"),
                classes="row",
            )
            yield Label("Audio format (when audio only)", classes="option-label")
            yield Select(
                (
                    ("MP3", "mp3"),
                    ("AAC", "aac"),
                    ("Opus", "opus"),
                    ("M4A", "m4a"),
                ),
                id="audio_fmt",
                allow_blank=False,
                disabled=True,
            )
            yield Label("Subtitles (space toggles; multi-select)", classes="option-label")
            yield SelectionList(
                *((lang, lang) for lang in self.video.subtitle_langs),
                id="subs",
            )
            yield Horizontal(
                Button("Download", variant="success", id="download"),
                Button("Quit", variant="error", id="quit"),
                classes="row",
            )
        yield Footer()

    def _title_markup(self) -> str:
        title = self.video.title.replace("[", "\\[")
        return title

    def _resolution_options(self) -> tuple[tuple[str, str], ...]:
        items: list[tuple[str, str]] = [("Best available", "best")]
        for h in self.video.heights:
            items.append((f"{h}p", str(h)))
        return tuple(items)

    def on_mount(self) -> None:
        self.query_one("#audio_fmt", Select).disabled = True

    @on(Switch.Changed, "#audio_only")
    def _toggle_audio_mode(self, event: Switch.Changed) -> None:
        audio_only = event.value
        self.query_one("#resolution", Select).disabled = audio_only
        self.query_one("#merge", Select).disabled = audio_only
        self.query_one("#audio_fmt", Select).disabled = not audio_only

    @on(Button.Pressed, "#quit")
    def _quit_pressed(self) -> None:
        self.app.exit()

    @on(Button.Pressed, "#download")
    def _download_pressed(self) -> None:
        res_select = self.query_one("#resolution", Select)
        merge_select = self.query_one("#merge", Select)
        audio_fmt = self.query_one("#audio_fmt", Select)
        audio_only = self.query_one("#audio_only", Switch).value

        res_val = str(res_select.value)
        height_cap: int | None = None if res_val == "best" else int(res_val)
        merge_fmt = str(merge_select.value)

        audio_ext = str(audio_fmt.value)

        sub_langs: list[str] = list(
            self.query_one("#subs", SelectionList).selected,
        )

        choices = DownloadChoices(
            output_dir=self.output_dir,
            height_cap=height_cap,
            merge_format=merge_fmt,
            audio_only=audio_only,
            audio_format=audio_ext,
            subtitle_langs=tuple(sub_langs),
        )
        self.app.push_screen(DownloadScreen(self.video, choices))


class DownloadScreen(Screen[None]):
    """Run yt-dlp with a live status line."""

    def __init__(self, video: VideoInfo, choices: DownloadChoices) -> None:
        super().__init__()
        self.video = video
        self.choices = choices

    def compose(self) -> ComposeResult:
        yield Header()
        with ScrollableContainer(id="card"):
            yield Static("[b]Download in progress[/b]")
            yield ProgressBar(total=100, show_eta=False, id="bar")
            yield Static("", id="dl_status")
        yield Footer()

    def on_mount(self) -> None:
        bar = self.query_one("#bar", ProgressBar)
        status = self.query_one("#dl_status", Static)
        app = self.app

        def on_progress(message: str) -> None:
            def update_ui() -> None:
                status.update(message)

            app.call_from_thread(update_ui)

        def task() -> None:
            try:
                download(self.video, self.choices, on_progress)
                app.call_from_thread(self._finish_ok)
            except Exception as e:
                app.call_from_thread(self._finish_err, str(e))

        threading.Thread(target=task, daemon=True).start()

        # Indeterminate feel until we parse percentages in hooks (optional enhancement).
        bar.progress = 0.0

    def _finish_ok(self) -> None:
        self.app.pop_screen()
        self.app.push_screen(MessageScreen("Done", "Download finished."))

    def _finish_err(self, message: str) -> None:
        self.app.pop_screen()
        self.app.push_screen(MessageScreen("Error", message))


class MessageScreen(Screen[None]):
    """Simple modal-style message with dismiss."""

    def __init__(self, heading: str, body: str) -> None:
        super().__init__()
        self.heading = heading
        self.body = body

    def compose(self) -> ComposeResult:
        yield Header()
        with Vertical(id="card"):
            yield Static(f"[b]{self.heading}[/b]\n\n{self.body}")
            yield Button("Close", id="close")
        yield Footer()

    @on(Button.Pressed, "#close")
    def _close(self) -> None:
        self.app.exit()
