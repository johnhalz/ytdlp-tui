//! Interactive TUI for yt-dlp (shells out to the `yt-dlp` binary).

mod app;
mod models;
mod ytdlp;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ytdlp-tui", version, about)]
struct Cli {
    /// Video URL passed through to yt-dlp.
    #[arg(value_name = "URL")]
    url: String,

    #[arg(short = 'o', long = "output-dir")]
    output_dir: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let out = cli
        .output_dir
        .unwrap_or_else(|| std::env::current_dir().expect("current directory"));
    let out = out.canonicalize().unwrap_or(out);
    app::run_tui(cli.url, out)?;
    Ok(())
}
