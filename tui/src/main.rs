//! agent-chat-tui: a terminal client for Agent Chat. Talks to the running app's HTTP API and
//! event streams, so the web UI and this one share every chat, agent and setting.

mod api;
mod app;
mod commands;
mod forms;
mod md;
mod text;
mod theme;
mod trace;
mod ui;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::cursor::Show;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

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
    if let Some(t) = theme::Theme::named(&app.cfg.theme).filter(|_| !args.plain && !args.light) {
        app.theme = t; // the last /theme, unless a flag picks one
    }
    app.bell = !args.quiet;
    if let Err(e) = app.boot().await {
        eprintln!("Can't reach Agent Chat at {}: {e}\nStart it with ./run.sh, or pass --url.", args.url);
        std::process::exit(1);
    }
    if let Some(c) = args.chat {
        app.open_chat(Some(c)).await;
        // what is typed first goes into its message box (the sidebar would take it as shortcuts)
        app.focus = if app.in_chat() { app::Focus::Composer } else { app::Focus::Messages };
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    // a panic would leave the shell in raw mode on the alternate screen, the mouse captured. Only
    // the main thread's ends the app; one in a background task leaves it running (and drawing)
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            let _ = restore(&mut io::stdout());
        }
        hook(info);
    }));
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal, &mut app).await;
    restore(terminal.backend_mut())?;
    result
}

/// The terminal back as the shell had it: cooked mode, the main screen, no mouse capture or
/// bracketed paste, a visible cursor.
fn restore(out: &mut impl io::Write) -> io::Result<()> {
    let _ = disable_raw_mode();
    execute!(out, LeaveAlternateScreen, DisableMouseCapture, DisableBracketedPaste, Show)
}

type Stamped = (Instant, io::Result<TermEvent>);

/// Terminal input, read on its own thread and stamped as it arrives (so a keystroke paste can be
/// told from typing even while the app is busy), sent on in batches of whatever came together.
/// A plain thread rather than a task: it blocks in `read`, and the process exits around it.
fn read_input() -> mpsc::UnboundedReceiver<Vec<Stamped>> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || loop {
        let first = event::read();
        let failed = first.is_err();
        let mut batch = vec![(Instant::now(), first)];
        while !failed && matches!(event::poll(Duration::ZERO), Ok(true)) {
            batch.push((Instant::now(), event::read()));
        }
        if tx.send(batch).is_err() || failed {
            break;
        }
    });
    rx
}

async fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut app::App) -> Result<()> {
    let mut input = read_input();
    let mut queue: VecDeque<Stamped> = VecDeque::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut poll = tokio::time::interval(Duration::from_secs(10));
    loop {
        if app.burst_deadline().is_some_and(|d| d <= Instant::now()) && guard(&mut input, &mut queue, app.flush_burst()).await {
            return Ok(());
        }
        terminal.draw(|f| {
            ui::draw(f, app);
            text::scrub(f.buffer_mut());
        })?;
        let flush_at = app.burst_deadline();
        let quit = tokio::select! {
            batch = input.recv() => match batch {
                Some(b) => {
                    queue.extend(b);
                    false
                }
                None => break,
            },
            inc = app.rx.recv() => match inc {
                Some(inc) => guard(&mut input, &mut queue, app.on_incoming(inc)).await,
                None => false,
            },
            _ = tick.tick() => {
                app.tick();
                false
            }
            _ = poll.tick() => {
                app.poll_server_soon();
                false
            }
            _ = tokio::time::sleep_until(flush_at.unwrap_or_else(Instant::now).into()), if flush_at.is_some() => guard(&mut input, &mut queue, app.flush_burst()).await,
        };
        if quit {
            return Ok(());
        }
        // handle everything else that is already here (input and server events), then draw once
        loop {
            while let Ok(b) = input.try_recv() {
                queue.extend(b);
            }
            let quit = if let Some((at, ev)) = queue.pop_front() {
                let Ok(ev) = ev else { return Ok(()) };
                let next = queue.iter().find(|(_, e)| matches!(e, Ok(TermEvent::Key(_)))).map(|(t, _)| *t);
                guard(&mut input, &mut queue, app.on_input(at, ev, next)).await
            } else if let Ok(inc) = app.rx.try_recv() {
                guard(&mut input, &mut queue, app.on_incoming(inc)).await
            } else {
                break;
            };
            if quit || app.quit {
                return Ok(());
            }
        }
        if app.quit {
            break;
        }
    }
    Ok(())
}

/// Wait for one of the app's handlers, reading the terminal meanwhile: a handler can be waiting
/// on the app (a request, until it times out), and the keys typed then are kept for after it,
/// except Ctrl+Q, which quits at once (the handler and its request dropped). True to quit.
async fn guard(input: &mut mpsc::UnboundedReceiver<Vec<Stamped>>, queue: &mut VecDeque<Stamped>, work: impl std::future::Future<Output = ()>) -> bool {
    tokio::pin!(work);
    let mut open = true;
    loop {
        tokio::select! {
            _ = &mut work => return false,
            batch = input.recv(), if open => match batch {
                Some(b) => {
                    let quit = quits(&b);
                    queue.extend(b);
                    if quit {
                        return true;
                    }
                }
                None => open = false,
            },
        }
    }
}

/// A Ctrl+Q pressed on its own (one inside a keystroke paste comes with the rest of the paste).
fn quits(batch: &[Stamped]) -> bool {
    matches!(batch, [(_, Ok(TermEvent::Key(k)))] if k.kind != KeyEventKind::Release && k.code == KeyCode::Char('q') && k.modifiers == KeyModifiers::CONTROL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(c: char, m: KeyModifiers) -> Stamped {
        (Instant::now(), Ok(TermEvent::Key(KeyEvent::new(KeyCode::Char(c), m))))
    }

    #[tokio::test]
    async fn ctrl_q_quits_while_a_handler_waits_on_a_stalled_app() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut queue = VecDeque::new();
        // (a request to an app that doesn't answer)
        let stalled = tokio::time::sleep(Duration::from_secs(60));
        tx.send(vec![key('k', KeyModifiers::CONTROL)]).unwrap();
        tx.send(vec![key('a', KeyModifiers::NONE), key('q', KeyModifiers::CONTROL)]).unwrap(); // (inside a paste)
        tx.send(vec![key('q', KeyModifiers::CONTROL)]).unwrap();
        let t0 = Instant::now();
        assert!(guard(&mut rx, &mut queue, stalled).await, "Ctrl+Q quits");
        assert!(t0.elapsed() < Duration::from_secs(1));
        assert_eq!(queue.len(), 4, "the keys typed meanwhile are kept for after");
        // a handler that ends: the keys wait for the loop, nothing quits
        tx.send(vec![key('x', KeyModifiers::NONE)]).unwrap();
        assert!(!guard(&mut rx, &mut VecDeque::new(), tokio::time::sleep(Duration::from_millis(50))).await);
        assert!(!quits(&[key('q', KeyModifiers::NONE)]) && !quits(&[key('c', KeyModifiers::CONTROL)]));
    }

    #[test]
    fn restoring_undoes_every_mode_the_app_turns_on() {
        let mut out = vec![];
        super::restore(&mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        for (seq, what) in [("\x1b[?1049l", "alternate screen"), ("\x1b[?1000l", "mouse capture"), ("\x1b[?1006l", "SGR mouse"), ("\x1b[?2004l", "bracketed paste"), ("\x1b[?25h", "cursor")] {
            assert!(out.contains(seq), "{what} not undone: {out:?}");
        }
    }
}
