//! Ratatui event loop and screens.

use crate::models::{DownloadChoices, VideoInfo, AUDIO_FORMATS, MERGE_FORMATS};
use crate::ytdlp;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph};
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

pub fn run_tui(url: String, output_dir: PathBuf) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let r = run_ui_loop(&mut terminal, rt, url, output_dir);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    r
}

enum Screen {
    Loading,
    Selector(SelectorState),
    Downloading {
        status_line: String,
        pct: Option<f64>,
        prog_rx: mpsc::Receiver<String>,
        done_rx: mpsc::Receiver<Result<(), String>>,
    },
    Message { title: String, body: String },
}

struct SelectorState {
    video: VideoInfo,
    /// 0 = best, k = video.heights[k - 1]
    resolution_idx: usize,
    merge_idx: usize,
    audio_only: bool,
    audio_fmt_idx: usize,
    sub_cursor: usize,
    subs_on: Vec<bool>,
    focus: Focus,
}

#[derive(Clone, Copy)]
enum Focus {
    Resolution,
    Merge,
    AudioOnly,
    AudioFmt,
    Subtitles,
    Download,
    Quit,
}

impl Focus {
    fn next(self) -> Focus {
        match self {
            Focus::Resolution => Focus::Merge,
            Focus::Merge => Focus::AudioOnly,
            Focus::AudioOnly => Focus::AudioFmt,
            Focus::AudioFmt => Focus::Subtitles,
            Focus::Subtitles => Focus::Download,
            Focus::Download => Focus::Quit,
            Focus::Quit => Focus::Resolution,
        }
    }

    fn prev(self) -> Focus {
        match self {
            Focus::Resolution => Focus::Quit,
            Focus::Merge => Focus::Resolution,
            Focus::AudioOnly => Focus::Merge,
            Focus::AudioFmt => Focus::AudioOnly,
            Focus::Subtitles => Focus::AudioFmt,
            Focus::Download => Focus::Subtitles,
            Focus::Quit => Focus::Download,
        }
    }
}

fn run_ui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rt: tokio::runtime::Runtime,
    url: String,
    output_dir: PathBuf,
) -> Result<()> {
    let (load_tx, load_rx) = mpsc::channel();
    let u = url.clone();
    rt.spawn(async move {
        let r = ytdlp::fetch_video_info(&u).await;
        let _ = load_tx.send(r.map_err(|e| e.to_string()));
    });

    let mut screen = Screen::Loading;

    'outer: loop {
        terminal.draw(|f| {
            let area = f.area();
            match &screen {
                Screen::Loading => {
                    let p = Paragraph::new(format!("Loading metadata…\n\n{url}"))
                        .block(Block::default().borders(Borders::ALL).title("ytdlp-tui"));
                    f.render_widget(p, area);
                }
                Screen::Selector(s) => draw_selector(f, area, s, &output_dir),
                Screen::Downloading {
                    status_line, pct, ..
                } => {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Length(3), Constraint::Min(0)])
                        .split(area);
                    let p = Paragraph::new("Download in progress")
                        .block(Block::default().borders(Borders::ALL).title("ytdlp-tui"));
                    f.render_widget(p, chunks[0]);
                    let label = status_line.as_str();
                    let ratio = pct.map(|p| p / 100.0).unwrap_or(0.0);
                    let g = Gauge::default()
                        .block(Block::default().borders(Borders::ALL))
                        .gauge_style(Style::default().fg(Color::Green))
                        .ratio(ratio.clamp(0.0, 1.0))
                        .label(label);
                    f.render_widget(g, chunks[1]);
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
                    Ok(v) => {
                        let n = v.subtitle_langs.len();
                        screen = Screen::Selector(SelectorState {
                            video: v,
                            resolution_idx: 0,
                            merge_idx: 0,
                            audio_only: false,
                            audio_fmt_idx: 0,
                            sub_cursor: 0,
                            subs_on: vec![false; n],
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
            while let Ok(msg) = prog_rx.try_recv() {
                if let Some(rest) = msg.strip_prefix("Downloading… ") {
                    let trimmed = rest.trim();
                    if let Ok(p) = trimmed.trim_end_matches('%').trim().parse::<f64>() {
                        *pct = Some(p);
                    }
                }
                *status_line = msg;
            }
            if let Ok(done) = done_rx.try_recv() {
                match done {
                    Ok(()) => {
                        screen = Screen::Message {
                            title: "Done".into(),
                            body: "Download finished.".into(),
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

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match &mut screen {
            Screen::Loading => {}
            Screen::Message { .. } => {
                if matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')
                ) {
                    break 'outer;
                }
            }
            Screen::Downloading { .. } => {}
            Screen::Selector(s) => {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break 'outer,
                    KeyCode::Tab => s.focus = s.focus.next(),
                    KeyCode::BackTab => s.focus = s.focus.prev(),
                    KeyCode::Char(' ') => match s.focus {
                        Focus::AudioOnly => s.audio_only = !s.audio_only,
                        Focus::Subtitles if !s.video.subtitle_langs.is_empty() => {
                            let i = s.sub_cursor.min(s.subs_on.len().saturating_sub(1));
                            if i < s.subs_on.len() {
                                s.subs_on[i] = !s.subs_on[i];
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
                        Focus::Quit => break 'outer,
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

fn adjust_selector(s: &mut SelectorState, delta: i32) {
    match s.focus {
        Focus::Resolution if !s.audio_only => {
            let max = s.video.heights.len();
            let i = (s.resolution_idx as i32 + delta).clamp(0, max as i32) as usize;
            s.resolution_idx = i;
        }
        Focus::Merge if !s.audio_only => {
            let max = MERGE_FORMATS.len() - 1;
            let i = (s.merge_idx as i32 + delta).clamp(0, max as i32) as usize;
            s.merge_idx = i;
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
        _ => {}
    }
}

fn build_choices(s: &SelectorState, output_dir: PathBuf) -> DownloadChoices {
    let height_cap = if s.audio_only || s.resolution_idx == 0 {
        None
    } else {
        Some(s.video.heights[s.resolution_idx - 1])
    };
    let merge_format = MERGE_FORMATS[s.merge_idx].to_string();
    let audio_format = AUDIO_FORMATS[s.audio_fmt_idx].to_string();
    let mut subtitle_langs = Vec::new();
    for (i, lang) in s.video.subtitle_langs.iter().enumerate() {
        if s.subs_on.get(i) == Some(&true) {
            subtitle_langs.push(lang.clone());
        }
    }
    DownloadChoices {
        output_dir,
        height_cap,
        merge_format,
        audio_only: s.audio_only,
        audio_format,
        subtitle_langs,
    }
}

fn draw_selector(f: &mut Frame, area: Rect, s: &SelectorState, output_dir: &Path) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(4),
        ])
        .split(area);

    f.render_widget(
        Paragraph::new(s.video.title.as_str()).block(Block::default().borders(Borders::ALL)),
        chunks[0],
    );

    let mut meta = format!(
        "URL: {}\nSave to: {}",
        s.video.url,
        output_dir.display()
    );
    if let Some(ref t) = s.video.thumbnail {
        meta.push_str(&format!("\nThumbnail: {t}"));
    }
    f.render_widget(
        Paragraph::new(meta).block(Block::default().borders(Borders::ALL)),
        chunks[1],
    );

    let res_items: Vec<ListItem> = std::iter::once(ListItem::new("Best available"))
        .chain(s.video.heights.iter().map(|h| ListItem::new(format!("{h}p"))))
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
        chunks[4],
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
        f.render_stateful_widget(af_list, chunks[5], &mut af_state);
    } else {
        f.render_widget(
            List::new(vec![ListItem::new("(enable audio only)")]).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Audio format")
                    .style(Style::default().fg(Color::DarkGray)),
            ),
            chunks[5],
        );
    }

    let sub_border = if matches!(s.focus, Focus::Subtitles) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let sub_lines: Vec<ListItem> = if s.video.subtitle_langs.is_empty() {
        vec![ListItem::new("(no subtitles reported)")]
    } else {
        s.video
            .subtitle_langs
            .iter()
            .enumerate()
            .map(|(i, lang)| {
                let on = s.subs_on.get(i).copied().unwrap_or(false);
                let mark = if on { "[x]" } else { "[ ]" };
                let hl = if i == s.sub_cursor { ">> " } else { "   " };
                ListItem::new(format!("{hl}{mark} {lang}"))
            })
            .collect()
    };
    f.render_widget(
        List::new(sub_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(sub_border)
                .title("Subtitles (↑↓ Space toggles)"),
        ),
        chunks[6],
    );

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
    .block(Block::default().borders(Borders::ALL).border_style(action_border));
    f.render_widget(actions, chunks[7]);
}
