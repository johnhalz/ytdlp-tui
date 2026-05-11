//! Ratatui event loop and screens.

use crate::models::{DownloadChoices, VideoInfo, VideoPick, AUDIO_FORMATS, MERGE_FORMATS};
use crate::sponsorblock::{self, SponsorSegment};
use crate::ytdlp;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
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

/// Lines reserved for the selector (`draw_selector` vertical constraints sum to this).
const SELECTOR_VIEWPORT_HEIGHT: u16 = 59;

#[derive(Debug)]
pub enum TuiExit {
    Quit,
    DownloadOk(Vec<PathBuf>),
}

pub fn run_tui(url: String, output_dir: PathBuf) -> Result<TuiExit> {
    let rt = tokio::runtime::Runtime::new()?;

    enable_raw_mode()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(SELECTOR_VIEWPORT_HEIGHT),
        },
    )?;

    let r = run_ui_loop(&mut terminal, rt, url, output_dir);

    disable_raw_mode()?;

    r
}

enum Screen {
    Loading,
    Selector(SelectorState),
    Downloading {
        status_line: String,
        pct: Option<f64>,
        prog_rx: mpsc::Receiver<String>,
        done_rx: mpsc::Receiver<Result<Vec<PathBuf>, String>>,
        choices: DownloadChoices,
    },
    PostProcessing {
        status_line: String,
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
    /// Parallel to `sponsor_segments`: include this range in the ffmpeg cut step.
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

    let exit = 'outer: loop {
        terminal.draw(|f| {
            let area = f.area();
            match &screen {
                Screen::Loading => {
                    let p = Paragraph::new(format!(
                        "Loading metadata and SponsorBlock segments…\n\n{url}"
                    ))
                    .block(Block::default().borders(Borders::ALL).title("ytdlp-tui"));
                    f.render_widget(p, area);
                }
                Screen::Selector(s) => draw_selector(f, area, s, &output_dir),
                Screen::Downloading {
                    status_line, pct, ..
                } => {
                    let block = Block::default().borders(Borders::ALL).title("ytdlp-tui");
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
                Screen::PostProcessing { status_line, .. } => {
                    let p = Paragraph::new(status_line.as_str())
                        .block(Block::default().borders(Borders::ALL).title("ytdlp-tui"));
                    f.render_widget(p, area);
                }
                Screen::Message { title, body } => {
                    let p = Paragraph::new(format!(
                        "{title}\n\n{body}\n\nPress Enter, Esc, or q to close."
                    ))
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
            ..
        } = &mut screen
        {
            while let Ok(msg) = prog_rx.try_recv() {
                if let Some(rest) = msg.strip_prefix("Downloading… ") {
                    let trimmed = rest.trim();
                    if let Ok(p) = trimmed.trim_end_matches('%').trim().parse::<f64>() {
                        *pct = Some(p);
                    }
                }
                *status_line = msg;
            }
        }

        if let Screen::Downloading { done_rx, choices, .. } = &mut screen {
            if let Ok(done) = done_rx.try_recv() {
                match done {
                    Ok(paths) => {
                        let c = choices.clone();
                        if c.cut_segments.is_empty()
                            || !paths.iter().any(|p| is_probably_video_path(p))
                        {
                            break 'outer TuiExit::DownloadOk(paths);
                        }
                        let (post_done_tx, post_done_rx) = mpsc::channel();
                        let post_paths = paths.clone();
                        rt.spawn(async move {
                            let ch = c.chapters.clone();
                            let cuts = c.cut_segments.clone();
                            for p in &post_paths {
                                if !is_probably_video_path(p) {
                                    continue;
                                }
                                if let Err(e) = ytdlp::cut_segments(p, &cuts, &ch).await {
                                    let _ = post_done_tx.send(Err(e.to_string()));
                                    return;
                                }
                            }
                            let _ = post_done_tx.send(Ok(post_paths));
                        });
                        screen = Screen::PostProcessing {
                            status_line: "Removing selected segments (ffmpeg)…".into(),
                            done_rx: post_done_rx,
                        };
                    }
                    Err(msg) => {
                        screen = Screen::Message {
                            title: "Error".into(),
                            body: msg,
                        };
                    }
                }
            }
        }

        if let Screen::PostProcessing { done_rx, .. } = &mut screen {
            if let Ok(r) = done_rx.try_recv() {
                match r {
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

                match &mut screen {
                    Screen::Loading => {}
                    Screen::Message { .. } => {
                        if matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
                            break 'outer TuiExit::Quit;
                        }
                    }
                    Screen::Downloading { .. } => {}
                    Screen::PostProcessing { .. } => {}
                    Screen::Selector(s) => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break 'outer TuiExit::Quit,
                        KeyCode::Tab => s.focus = s.focus.next(),
                        KeyCode::BackTab => s.focus = s.focus.prev(),
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
                                let dl_choices = choices.clone();
                                let (prog_tx, prog_rx) = mpsc::channel();
                                let (done_tx, done_rx) = mpsc::channel();
                                let video = s.video.clone();
                                rt.spawn(async move {
                                    let r =
                                        ytdlp::run_download(&video, &dl_choices, prog_tx).await;
                                    let _ = done_tx.send(r.map_err(|e| e.to_string()));
                                });
                                screen = Screen::Downloading {
                                    status_line: "Starting download…".into(),
                                    pct: None,
                                    prog_rx,
                                    done_rx,
                                    choices,
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
        let id = s.video.variants[s.resolution_idx - 1]
            .video_format_id
            .clone();
        VideoPick::ByFormatId {
            video_format_id: id,
        }
    };
    let merge_format = MERGE_FORMATS[s.merge_idx].to_string();
    let audio_track = if s.dub_idx == 0 {
        None
    } else {
        Some(
            s.video.audio_tracks[s.dub_idx - 1]
                .format_id
                .clone(),
        )
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
        chapters: s.video.chapters.clone(),
        cut_segments,
    }
}

fn draw_selector(f: &mut Frame, area: Rect, s: &SelectorState, output_dir: &Path) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(3),
            Constraint::Length(8),
            Constraint::Length(4),
        ])
        .split(area);

    f.render_widget(
        Paragraph::new(s.video.title.as_str()).block(Block::default().borders(Borders::ALL)),
        chunks[0],
    );

    let mut meta = format!("URL: {}\nSave to: {}", s.video.url, output_dir.display());
    if let Some(ref t) = s.video.thumbnail {
        meta.push_str(&format!("\nThumbnail: {t}"));
    }
    f.render_widget(
        Paragraph::new(meta).block(Block::default().borders(Borders::ALL)),
        chunks[1],
    );

    let res_items: Vec<ListItem> = std::iter::once(ListItem::new("Best available"))
        .chain(
            s.video
                .variants
                .iter()
                .map(|v| ListItem::new(v.label())),
        )
        .collect();
    let res_border = if matches!(s.focus, Focus::Resolution) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let mut res_state = ratatui::widgets::ListState::default();
    res_state.select(Some(s.resolution_idx));
    let res_list = List::new(res_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(res_border)
                .title("Resolution (↑↓ when focused)"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    if !s.audio_only {
        f.render_stateful_widget(res_list, chunks[2], &mut res_state);
    } else {
        f.render_widget(
            List::new(vec![ListItem::new("(disabled)")]).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Resolution (N/A — audio only)")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            chunks[2],
        );
    }

    let merge_items: Vec<ListItem> = MERGE_FORMATS
        .iter()
        .map(|x| ListItem::new((*x).to_string()))
        .collect();
    let merge_border = if matches!(s.focus, Focus::Merge) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let mut merge_state = ratatui::widgets::ListState::default();
    merge_state.select(Some(s.merge_idx));
    let merge_list = List::new(merge_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(merge_border)
                .title("Container"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    if !s.audio_only {
        f.render_stateful_widget(merge_list, chunks[3], &mut merge_state);
    } else {
        f.render_widget(
            List::new(vec![ListItem::new("(disabled)")]).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Container (N/A — audio only)")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            chunks[3],
        );
    }

    let dub_border = if matches!(s.focus, Focus::Dub) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    if s.video.audio_tracks.is_empty() {
        f.render_widget(
            Paragraph::new("(no alternate audio tracks reported)").block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(dub_border)
                    .title("Audio track (↑↓ when focused)")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            chunks[4],
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
                    .title("Audio track (↑↓ when focused)"),
            )
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("> ");
        f.render_stateful_widget(dub_list, chunks[4], &mut dub_state);
    }

    let ao_border = if matches!(s.focus, Focus::AudioOnly) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(format!(
            "Audio only: {}  |  Focus here and press Space to toggle",
            if s.audio_only { "yes" } else { "no" }
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(ao_border),
        ),
        chunks[5],
    );

    let audio_items: Vec<ListItem> = AUDIO_FORMATS
        .iter()
        .map(|x| ListItem::new((*x).to_string()))
        .collect();
    let af_border = if matches!(s.focus, Focus::AudioFmt) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let mut af_state = ratatui::widgets::ListState::default();
    af_state.select(Some(s.audio_fmt_idx));
    let af_list = List::new(audio_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(af_border)
                .title("Audio format"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    if s.audio_only {
        f.render_stateful_widget(af_list, chunks[6], &mut af_state);
    } else {
        f.render_widget(
            List::new(vec![ListItem::new("(enable audio only)")]).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Audio format")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            chunks[6],
        );
    }

    let sub_border = if matches!(s.focus, Focus::Subtitles) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    if s.video.subtitle_langs.is_empty() {
        f.render_widget(
            List::new(vec![ListItem::new("(no subtitles reported)")]).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(sub_border)
                    .title("Subtitles (↑↓ Space toggles)"),
            ),
            chunks[7],
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
                    .title("Subtitles (↑↓ Space toggles)"),
            )
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("> ");
        f.render_stateful_widget(sub_list, chunks[7], &mut sub_state);
    }

    let ec_border = if matches!(s.focus, Focus::EmbedChapters) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(format!(
            "Embed chapters: {}  |  Focus here and press Space to toggle",
            if s.embed_chapters { "yes" } else { "no" }
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(ec_border),
        ),
        chunks[8],
    );

    let sb_border = if matches!(s.focus, Focus::SponsorBlock) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    if s.sponsor_segments.is_empty() {
        f.render_widget(
            Paragraph::new("(no SponsorBlock segments for this video)").block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(sb_border)
                    .title("SponsorBlock (↑↓ Space toggles cut)")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            chunks[9],
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
                    .title("SponsorBlock (↑↓ Space toggles cut)"),
            )
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("> ");
        f.render_stateful_widget(sb_list, chunks[9], &mut sb_state);
    }

    let action_border = if matches!(s.focus, Focus::Download | Focus::Quit) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let focus_hint = match s.focus {
        Focus::Download => "Download — press Enter",
        Focus::Quit => "Quit — press Enter",
        _ => "Tab to Download / Quit, then Enter",
    };
    let actions = Paragraph::new(format!(
        "Actions\n\n{focus_hint}\n\n(q or Esc) quit from any screen except message"
    ))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(action_border),
    );
    f.render_widget(actions, chunks[10]);
}

fn format_timestamp(sec: f64) -> String {
    let s = sec.max(0.0);
    let frac = ((s - s.floor()) * 100.0).round() as u32;
    let t = s.floor() as u64;
    let h = t / 3600;
    let m = (t % 3600) / 60;
    let s0 = t % 60;
    format!("{h:02}:{m:02}:{s0:02}.{frac:02}")
}

fn is_probably_video_path(p: &Path) -> bool {
    const EXT: &[&str] = &["mkv", "mp4", "webm", "m4v", "mov", "avi"];
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXT.iter().any(|x| x.eq_ignore_ascii_case(e)))
}
