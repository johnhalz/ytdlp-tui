//! Ratatui event loop and screens.

use crate::models::{DownloadChoices, VideoInfo, VideoPick, AUDIO_FORMATS, MERGE_FORMATS};
use crate::sponsorblock::{self, SponsorSegment};
use crate::ytdlp::{self, DlEvent};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::symbols::{self, line};
use ratatui::widgets::{Block, Borders, LineGauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{TerminalOptions, Viewport};
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Lines wanted for the selector (header + tallest column in `draw_selector`); capped to the terminal height.
const SELECTOR_VIEWPORT_HEIGHT: u16 = 32;

#[derive(Debug)]
pub enum TuiExit {
    Quit,
    DownloadOk(Vec<PathBuf>),
}

pub fn run_tui(url: String, output_dir: PathBuf) -> Result<TuiExit> {
    let rt = tokio::runtime::Runtime::new()?;

    // Restore the terminal before the panic message prints, or the shell is left in raw mode.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        default_hook(info);
    }));

    let rows = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(SELECTOR_VIEWPORT_HEIGHT);
    enable_raw_mode()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(SELECTOR_VIEWPORT_HEIGHT.min(rows)),
        },
    )?;

    let r = run_ui_loop(&mut terminal, rt, url, output_dir);

    // Wipe the viewport and park the cursor at its top so the caller's output isn't printed over the frame.
    let _ = terminal.clear();
    disable_raw_mode()?;

    r
}

enum Screen {
    Loading,
    Selector(SelectorState),
    Downloading {
        status_line: String,
        pct: Option<f64>,
        prog_rx: mpsc::Receiver<DlEvent>,
        done_rx: mpsc::Receiver<Result<Vec<PathBuf>, String>>,
    },
    Message {
        title: String,
        body: String,
    },
}

struct SelectorState {
    video: VideoInfo,
    /// 0 = best, k = video.variants[k - 1]
    resolution_idx: usize,
    merge_idx: usize,
    /// 0 = original/default audio (`bestaudio`); k >= 1 maps to `video.audio_tracks[k - 1]`.
    dub_idx: usize,
    audio_only: bool,
    audio_fmt_idx: usize,
    sub_cursor: usize,
    subs_on: Vec<bool>,
    embed_chapters: bool,
    sponsor_segments: Vec<SponsorSegment>,
    /// Parallel to `sponsor_segments`: remove this range from the output.
    sponsor_cut: Vec<bool>,
    sponsor_cursor: usize,
    focus: Focus,
}

#[derive(Clone, Copy)]
enum Focus {
    Resolution,
    Merge,
    Dub,
    AudioOnly,
    AudioFmt,
    Subtitles,
    EmbedChapters,
    SponsorBlock,
    Download,
    Quit,
}

impl Focus {
    fn next(self) -> Focus {
        match self {
            Focus::Resolution => Focus::Merge,
            Focus::Merge => Focus::Dub,
            Focus::Dub => Focus::AudioOnly,
            Focus::AudioOnly => Focus::AudioFmt,
            Focus::AudioFmt => Focus::Subtitles,
            Focus::Subtitles => Focus::EmbedChapters,
            Focus::EmbedChapters => Focus::SponsorBlock,
            Focus::SponsorBlock => Focus::Download,
            Focus::Download => Focus::Quit,
            Focus::Quit => Focus::Resolution,
        }
    }

    fn prev(self) -> Focus {
        match self {
            Focus::Resolution => Focus::Quit,
            Focus::Merge => Focus::Resolution,
            Focus::Dub => Focus::Merge,
            Focus::AudioOnly => Focus::Dub,
            Focus::AudioFmt => Focus::AudioOnly,
            Focus::Subtitles => Focus::AudioFmt,
            Focus::EmbedChapters => Focus::Subtitles,
            Focus::SponsorBlock => Focus::EmbedChapters,
            Focus::Download => Focus::SponsorBlock,
            Focus::Quit => Focus::Download,
        }
    }

    /// Panels greyed out by the audio-only toggle are skipped by Tab.
    fn enabled(self, audio_only: bool) -> bool {
        match self {
            Focus::Resolution | Focus::Merge | Focus::Subtitles => !audio_only,
            Focus::AudioFmt => audio_only,
            _ => true,
        }
    }
}

fn run_ui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rt: tokio::runtime::Runtime,
    url: String,
    output_dir: PathBuf,
) -> Result<TuiExit> {
    let (load_tx, load_rx) = mpsc::channel();
    let u = url.clone();
    rt.spawn(async move {
        let (video_r, segs_r) = tokio::join!(
            ytdlp::fetch_video_info(&u),
            sponsorblock::fetch_segments(&u),
        );
        let res = match video_r {
            Err(e) => Err(e.to_string()),
            Ok(v) => {
                let segs = segs_r.unwrap_or_else(|_| Vec::new());
                Ok((v, segs))
            }
        };
        let _ = load_tx.send(res);
    });

    let mut screen = Screen::Loading;

    // Returning drops `rt`, which drops in-flight tasks; `kill_on_drop` then stops yt-dlp.
    let exit = 'outer: loop {
        terminal.draw(|f| {
            let area = f.area();
            match &screen {
                Screen::Loading => {
                    let p = Paragraph::new(format!(
                        "Loading metadata and SponsorBlock segments…\n\n{url}\n\n(q, Esc or Ctrl+C to cancel)"
                    ))
                    .block(Block::default().borders(Borders::ALL).title("ytdlp-tui"));
                    f.render_widget(p, area);
                }
                Screen::Selector(s) => draw_selector(f, area, s, &output_dir),
                Screen::Downloading {
                    status_line, pct, ..
                } => {
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .title("ytdlp-tui — q, Esc or Ctrl+C to cancel");
                    let inner = block.inner(area);
                    f.render_widget(block, area);
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(1),
                            Constraint::Length(1),
                        ])
                        .split(inner);
                    let path_text = format!("Save to: {}", output_dir.display());
                    let path_para = Paragraph::new(path_text).wrap(Wrap { trim: true });
                    f.render_widget(path_para, chunks[0]);
                    let ratio = pct.map(|p| p / 100.0).unwrap_or(0.0).clamp(0.0, 1.0);
                    let tqdm_line_set = line::Set {
                        horizontal: symbols::bar::FULL,
                        ..line::NORMAL
                    };
                    let lg = LineGauge::default()
                        .filled_style(Style::default().fg(Color::Green))
                        .unfilled_style(Style::default().fg(Color::DarkGray))
                        .line_set(tqdm_line_set)
                        .ratio(ratio)
                        .label(Line::from(status_line.as_str()));
                    f.render_widget(lg, chunks[1]);
                }
                Screen::Message { title, body } => {
                    let p = Paragraph::new(format!(
                        "{title}\n\n{body}\n\nPress Enter, Esc, or q to close."
                    ))
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title("ytdlp-tui"));
                    f.render_widget(p, area);
                }
            }
        })?;

        if matches!(screen, Screen::Loading) {
            if let Ok(r) = load_rx.try_recv() {
                match r {
                    Ok((v, segs)) => {
                        let n = v.subtitle_langs.len();
                        let sn = segs.len();
                        screen = Screen::Selector(SelectorState {
                            video: v,
                            resolution_idx: 0,
                            merge_idx: 0,
                            dub_idx: 0,
                            audio_only: false,
                            audio_fmt_idx: 0,
                            sub_cursor: 0,
                            subs_on: vec![false; n],
                            embed_chapters: true,
                            sponsor_segments: segs,
                            sponsor_cut: vec![false; sn],
                            sponsor_cursor: 0,
                            focus: Focus::Resolution,
                        });
                    }
                    Err(e) => {
                        screen = Screen::Message {
                            title: "Error".into(),
                            body: e,
                        };
                    }
                }
            }
        }

        if let Screen::Downloading {
            status_line,
            pct,
            prog_rx,
            done_rx,
        } = &mut screen
        {
            while let Ok(ev) = prog_rx.try_recv() {
                match ev {
                    DlEvent::Progress(p) => {
                        *pct = Some(p);
                        *status_line = format!("Downloading… {p:.1}%");
                    }
                    DlEvent::PostProcessing => {
                        *pct = Some(100.0);
                        *status_line = "Post-processing (merge / cut / embed)…".into();
                    }
                }
            }
            if let Ok(done) = done_rx.try_recv() {
                match done {
                    Ok(paths) => break 'outer TuiExit::DownloadOk(paths),
                    Err(msg) => {
                        screen = Screen::Message {
                            title: "Error".into(),
                            body: msg,
                        };
                    }
                }
            }
        }

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        match event::read()? {
            Event::Resize(_, _) => {
                terminal.autoresize()?;
            }
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                // Raw mode swallows SIGINT; treat Ctrl+C as quit everywhere.
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break 'outer TuiExit::Quit;
                }

                match &mut screen {
                    Screen::Loading | Screen::Downloading { .. } => {
                        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                            break 'outer TuiExit::Quit;
                        }
                    }
                    Screen::Message { .. } => {
                        if matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
                            break 'outer TuiExit::Quit;
                        }
                    }
                    Screen::Selector(s) => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break 'outer TuiExit::Quit,
                        KeyCode::Tab => {
                            s.focus = s.focus.next();
                            while !s.focus.enabled(s.audio_only) {
                                s.focus = s.focus.next();
                            }
                        }
                        KeyCode::BackTab => {
                            s.focus = s.focus.prev();
                            while !s.focus.enabled(s.audio_only) {
                                s.focus = s.focus.prev();
                            }
                        }
                        KeyCode::Char(' ') => match s.focus {
                            Focus::AudioOnly => s.audio_only = !s.audio_only,
                            Focus::EmbedChapters => s.embed_chapters = !s.embed_chapters,
                            Focus::Subtitles if !s.video.subtitle_langs.is_empty() => {
                                let i = s.sub_cursor.min(s.subs_on.len().saturating_sub(1));
                                if i < s.subs_on.len() {
                                    s.subs_on[i] = !s.subs_on[i];
                                }
                            }
                            Focus::SponsorBlock if !s.sponsor_segments.is_empty() => {
                                let i = s
                                    .sponsor_cursor
                                    .min(s.sponsor_cut.len().saturating_sub(1));
                                if i < s.sponsor_cut.len() {
                                    s.sponsor_cut[i] = !s.sponsor_cut[i];
                                }
                            }
                            _ => {}
                        },
                        KeyCode::Up => adjust_selector(s, -1),
                        KeyCode::Down => adjust_selector(s, 1),
                        KeyCode::Enter => match s.focus {
                            Focus::Download => {
                                let choices = build_choices(s, output_dir.clone());
                                let (prog_tx, prog_rx) = mpsc::channel();
                                let (done_tx, done_rx) = mpsc::channel();
                                let video = s.video.clone();
                                rt.spawn(async move {
                                    let r = ytdlp::run_download(&video, &choices, prog_tx).await;
                                    let _ = done_tx.send(r.map_err(|e| e.to_string()));
                                });
                                screen = Screen::Downloading {
                                    status_line: "Starting download…".into(),
                                    pct: None,
                                    prog_rx,
                                    done_rx,
                                };
                            }
                            Focus::Quit => break 'outer TuiExit::Quit,
                            _ => {}
                        },
                        _ => {}
                    },
                }
            }
            _ => {}
        }
    };

    Ok(exit)
}

fn adjust_selector(s: &mut SelectorState, delta: i32) {
    match s.focus {
        Focus::Resolution if !s.audio_only => {
            let max = s.video.variants.len();
            let i = (s.resolution_idx as i32 + delta).clamp(0, max as i32) as usize;
            s.resolution_idx = i;
        }
        Focus::Merge if !s.audio_only => {
            let max = MERGE_FORMATS.len() - 1;
            let i = (s.merge_idx as i32 + delta).clamp(0, max as i32) as usize;
            s.merge_idx = i;
        }
        Focus::Dub if !s.video.audio_tracks.is_empty() => {
            let max = s.video.audio_tracks.len();
            let i = (s.dub_idx as i32 + delta).clamp(0, max as i32) as usize;
            s.dub_idx = i;
        }
        Focus::AudioFmt if s.audio_only => {
            let max = AUDIO_FORMATS.len() - 1;
            let i = (s.audio_fmt_idx as i32 + delta).clamp(0, max as i32) as usize;
            s.audio_fmt_idx = i;
        }
        Focus::Subtitles if !s.video.subtitle_langs.is_empty() => {
            let max = s.video.subtitle_langs.len().saturating_sub(1);
            let i = (s.sub_cursor as i32 + delta).clamp(0, max as i32) as usize;
            s.sub_cursor = i;
        }
        Focus::SponsorBlock if !s.sponsor_segments.is_empty() => {
            let max = s.sponsor_segments.len().saturating_sub(1);
            let i = (s.sponsor_cursor as i32 + delta).clamp(0, max as i32) as usize;
            s.sponsor_cursor = i;
        }
        _ => {}
    }
}

fn build_choices(s: &SelectorState, output_dir: PathBuf) -> DownloadChoices {
    let video_pick = if s.audio_only || s.resolution_idx == 0 {
        VideoPick::Best
    } else {
        VideoPick::Variant(s.video.variants[s.resolution_idx - 1].clone())
    };
    let merge_format = MERGE_FORMATS[s.merge_idx].to_string();
    let audio_track = if s.dub_idx == 0 {
        None
    } else {
        Some(s.video.audio_tracks[s.dub_idx - 1].language.clone())
    };
    let audio_format = AUDIO_FORMATS[s.audio_fmt_idx].to_string();
    let mut subtitle_langs = Vec::new();
    for (i, lang) in s.video.subtitle_langs.iter().enumerate() {
        if s.subs_on.get(i) == Some(&true) {
            subtitle_langs.push(lang.clone());
        }
    }
    let mut cut_segments = Vec::new();
    for (i, seg) in s.sponsor_segments.iter().enumerate() {
        if s.sponsor_cut.get(i) == Some(&true) {
            cut_segments.push((seg.start, seg.end));
        }
    }
    DownloadChoices {
        output_dir,
        video_pick,
        merge_format,
        audio_track,
        audio_only: s.audio_only,
        audio_format,
        subtitle_langs,
        embed_chapters: s.embed_chapters,
        cut_segments,
    }
}

/// Which players can open each `MERGE_FORMATS` entry, given the codecs `download_args` sorts for.
fn container_label(fmt: &str) -> String {
    match fmt {
        "mp4" => "mp4  (QuickTime / IINA / VLC)".into(),
        other => format!("{other}  (IINA / VLC)"),
    }
}

fn border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

fn draw_selector(f: &mut Frame, area: Rect, s: &SelectorState, output_dir: &Path) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    // Min(0) on the last panel of each column lets short terminals shrink it instead of dropping panels.
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Min(3),
        ])
        .split(cols[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(4),
        ])
        .split(cols[1]);

    f.render_widget(
        Paragraph::new(format!(
            "URL: {}\nSave to: {}",
            s.video.url,
            output_dir.display()
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(s.video.title.as_str()).bold()),
        ),
        rows[0],
    );

    let mp4 = MERGE_FORMATS[s.merge_idx] == "mp4";
    let res_items: Vec<ListItem> = std::iter::once(ListItem::new(if mp4 {
        "Best available (H.264 preferred)"
    } else {
        "Best available"
    }))
    .chain(s.video.variants.iter().map(|v| {
        if mp4 && !v.h264 {
            ListItem::new(format!("{} · not QuickTime", v.label()))
        } else {
            ListItem::new(v.label())
        }
    }))
    .collect();
    let mut res_state = ListState::default();
    res_state.select(Some(s.resolution_idx));
    let res_list = List::new(res_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border(matches!(s.focus, Focus::Resolution)))
                .title("Resolution (↑↓)"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    if !s.audio_only {
        f.render_stateful_widget(res_list, left[0], &mut res_state);
    } else {
        f.render_widget(
            List::new(vec![ListItem::new("(disabled)")]).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Resolution (N/A — audio only)")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            left[0],
        );
    }

    let merge_items: Vec<ListItem> = MERGE_FORMATS
        .iter()
        .map(|x| ListItem::new(container_label(x)))
        .collect();
    let mut merge_state = ListState::default();
    merge_state.select(Some(s.merge_idx));
    let merge_list = List::new(merge_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border(matches!(s.focus, Focus::Merge)))
                .title("Container"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    if !s.audio_only {
        f.render_stateful_widget(merge_list, left[1], &mut merge_state);
    } else {
        f.render_widget(
            List::new(vec![ListItem::new("(disabled)")]).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Container (N/A — audio only)")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            left[1],
        );
    }

    let dub_border = border(matches!(s.focus, Focus::Dub));
    if s.video.audio_tracks.is_empty() {
        f.render_widget(
            Paragraph::new("(no alternate audio tracks reported)").block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(dub_border)
                    .title("Audio track (↑↓)")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            left[2],
        );
    } else {
        let dub_items: Vec<ListItem> = std::iter::once(ListItem::new("Original (default)"))
            .chain(
                s.video
                    .audio_tracks
                    .iter()
                    .map(|t| ListItem::new(t.label())),
            )
            .collect();
        let mut dub_state = ListState::default();
        dub_state.select(Some(s.dub_idx));
        let dub_list = List::new(dub_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(dub_border)
                    .title("Audio track (↑↓)"),
            )
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("> ");
        f.render_stateful_widget(dub_list, left[2], &mut dub_state);
    }

    f.render_widget(
        Paragraph::new(format!(
            "Audio only: {}  (Space)",
            if s.audio_only { "yes" } else { "no" }
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border(matches!(s.focus, Focus::AudioOnly))),
        ),
        left[3],
    );

    let audio_items: Vec<ListItem> = AUDIO_FORMATS
        .iter()
        .map(|x| ListItem::new((*x).to_string()))
        .collect();
    let mut af_state = ListState::default();
    af_state.select(Some(s.audio_fmt_idx));
    let af_list = List::new(audio_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border(matches!(s.focus, Focus::AudioFmt)))
                .title("Audio format"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    if s.audio_only {
        f.render_stateful_widget(af_list, left[4], &mut af_state);
    } else {
        f.render_widget(
            List::new(vec![ListItem::new("(enable audio only)")]).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Audio format")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            left[4],
        );
    }

    let sub_border = border(matches!(s.focus, Focus::Subtitles));
    if s.audio_only || s.video.subtitle_langs.is_empty() {
        let msg = if s.audio_only {
            "(N/A — audio only)"
        } else {
            "(no subtitles reported)"
        };
        f.render_widget(
            List::new(vec![ListItem::new(msg)]).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(sub_border)
                    .title("Subtitles, embedded (↑↓ Space)")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            right[0],
        );
    } else {
        let sub_items: Vec<ListItem> = s
            .video
            .subtitle_langs
            .iter()
            .enumerate()
            .map(|(i, lang)| {
                let on = s.subs_on.get(i).copied().unwrap_or(false);
                let mark = if on { "[x]" } else { "[ ]" };
                ListItem::new(format!("{mark} {lang}"))
            })
            .collect();
        let mut sub_state = ListState::default();
        sub_state.select(Some(s.sub_cursor));
        let sub_list = List::new(sub_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(sub_border)
                    .title("Subtitles, embedded (↑↓ Space)"),
            )
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("> ");
        f.render_stateful_widget(sub_list, right[0], &mut sub_state);
    }

    f.render_widget(
        Paragraph::new(format!(
            "Embed chapters: {}  (Space)",
            if s.embed_chapters { "yes" } else { "no" }
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border(matches!(s.focus, Focus::EmbedChapters))),
        ),
        right[1],
    );

    let sb_border = border(matches!(s.focus, Focus::SponsorBlock));
    if s.sponsor_segments.is_empty() {
        f.render_widget(
            Paragraph::new("(no SponsorBlock segments for this video)").block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(sb_border)
                    .title("SponsorBlock cuts (↑↓ Space)")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            right[2],
        );
    } else {
        let sb_items: Vec<ListItem> = s
            .sponsor_segments
            .iter()
            .enumerate()
            .map(|(i, seg)| {
                let on = s.sponsor_cut.get(i).copied().unwrap_or(false);
                let mark = if on { "[x]" } else { "[ ]" };
                ListItem::new(format!(
                    "{mark} {} – {}  •  {}",
                    format_timestamp(seg.start),
                    format_timestamp(seg.end),
                    seg.category
                ))
            })
            .collect();
        let mut sb_state = ListState::default();
        sb_state.select(Some(
            s.sponsor_cursor.min(s.sponsor_segments.len().saturating_sub(1)),
        ));
        let sb_list = List::new(sb_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(sb_border)
                    .title("SponsorBlock cuts (↑↓ Space)"),
            )
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("> ");
        f.render_stateful_widget(sb_list, right[2], &mut sb_state);
    }

    let button = |label: &str, focused: bool| {
        if focused {
            Span::styled(format!("[ {label} ]"), Style::default().fg(Color::Black).bg(Color::Yellow))
        } else {
            Span::raw(format!("[ {label} ]"))
        }
    };
    let actions = Paragraph::new(vec![
        Line::from(vec![
            button("Download", matches!(s.focus, Focus::Download)),
            Span::raw("  "),
            button("Quit", matches!(s.focus, Focus::Quit)),
        ]),
        Line::from("Tab/⇧Tab move · Enter run · q/Esc quit").dark_gray(),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border(matches!(s.focus, Focus::Download | Focus::Quit))),
    );
    f.render_widget(actions, right[3]);
}

fn format_timestamp(sec: f64) -> String {
    // Round once in centiseconds so e.g. 59.996 carries into the seconds instead of printing ".100".
    let cs = (sec.max(0.0) * 100.0).round() as u64;
    let (t, frac) = (cs / 100, cs % 100);
    let h = t / 3600;
    let m = (t % 3600) / 60;
    let s0 = t % 60;
    format!("{h:02}:{m:02}:{s0:02}.{frac:02}")
}

#[cfg(test)]
mod tests {
    use super::format_timestamp;

    #[test]
    fn timestamp_rounds_into_seconds() {
        assert_eq!(format_timestamp(59.996), "00:01:00.00");
        assert_eq!(format_timestamp(3723.5), "01:02:03.50");
    }
}
