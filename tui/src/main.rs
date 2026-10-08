//! agent-chat-tui: a terminal client for Agent Chat. Talks to the running app's HTTP API and
//! event streams, so the web UI and this one share every chat, agent and setting.

mod api;
mod app;
mod forms;
mod md;
mod theme;
mod trace;
mod ui;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, EventStream};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "agent-chat-tui", version, about = "Terminal client for Agent Chat")]
struct Args {
    /// The running Agent Chat app (also AGENT_CHAT_URL)
    #[arg(long, env = "AGENT_CHAT_URL", default_value = "http://127.0.0.1:8765")]
    url: String,
    /// Light palette (for light terminals)
    #[arg(long)]
    light: bool,
    /// Keep the terminal's own colors (no backgrounds)
    #[arg(long)]
    plain: bool,
    /// Open this chat on start
    #[arg(long)]
    chat: Option<String>,
    /// No terminal bell on approvals
    #[arg(long)]
    quiet: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let theme = if args.plain { theme::Theme::plain() } else if args.light { theme::Theme::light() } else { theme::Theme::dark() };
    let cfg_path = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join("agent-chat-tui").join("state.json");
    let api = api::Api::new(&args.url);
    let mut app = app::App::new(api.clone(), theme, cfg_path);
    app.bell = !args.quiet;
    if let Err(e) = app.boot().await {
        eprintln!("Can't reach Agent Chat at {}: {e}\nStart it with ./run.sh, or pass --url.", args.url);
        std::process::exit(1);
    }
    if let Some(c) = args.chat {
        app.open_chat(Some(c)).await;
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal, &mut app).await;
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableBracketedPaste)?;
    terminal.show_cursor()?;
    result
}

async fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut app::App) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut poll = tokio::time::interval(Duration::from_secs(10));
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        tokio::select! {
            ev = events.next() => {
                match ev {
                    Some(Ok(ev)) => app.on_term(ev).await,
                    Some(Err(_)) | None => break,
                }
            }
            inc = app.rx.recv() => {
                if let Some(inc) = inc {
                    app.on_incoming(inc).await;
                    // drain whatever else arrived, then draw once
                    while let Ok(more) = app.rx.try_recv() {
                        app.on_incoming(more).await;
                    }
                }
            }
            _ = tick.tick() => {
                app.tick();
            }
            _ = poll.tick() => {
                app.poll_server().await;
            }
        }
        if app.quit {
            break;
        }
    }
    Ok(())
}
