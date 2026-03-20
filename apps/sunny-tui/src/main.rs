//! sunny-tui — Sunny terminal UI (ratatui/crossterm).

use clap::Parser;
use crossterm::event::EventStream;
use std::time::Duration;
use sunny_store::{Database, SessionStore};
use tokio_stream::StreamExt;

mod agent;
mod app;
mod bridge;
mod interview_presenter;
mod thread;
mod ui;

use agent::spawn_agent_bridge;
use app::App;

/// CLI arguments for sunny-tui.
#[derive(Parser, Debug)]
#[command(name = "sunny-tui")]
#[command(about = "Sunny — AI coding assistant TUI")]
pub struct Args {
    /// Session ID to load (optional).
    #[arg(long)]
    pub session_id: Option<String>,

    /// Path to workspace root (auto-detected if omitted).
    #[arg(long)]
    pub workspace: Option<std::path::PathBuf>,
}

fn detect_workspace_root(override_path: Option<std::path::PathBuf>) -> std::path::PathBuf {
    if let Some(path) = override_path {
        return path;
    }
    // Try git rev-parse --show-toplevel
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if output.status.success() {
            if let Ok(path_str) = std::str::from_utf8(&output.stdout) {
                let trimmed = path_str.trim();
                if !trimmed.is_empty() {
                    return std::path::PathBuf::from(trimmed);
                }
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    // Route all logs to a file so they never bleed into the ratatui alternate screen.
    // Default path: ~/.local/share/sunny/sunny-tui.log
    // Override with SUNNY_TUI_LOG_DIR env var.
    let log_dir = std::env::var("SUNNY_TUI_LOG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::data_local_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join("sunny")
        });
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::never(&log_dir, "sunny-tui.log");
    let (non_blocking, _log_guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into()),
        ))
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    let args = Args::parse();
    let workspace_root = detect_workspace_root(args.workspace);
    let session_id = args.session_id;

    // Setup panic hook to restore terminal before panic output
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        default_hook(info);
    }));

    let local = tokio::task::LocalSet::new();

    local
        .run_until(async move {
            let mut terminal = ratatui::init();

            let mut app = App::new();

            if let Some(ref sid) = session_id {
                let sid = sid.clone();
                let sid_for_load = sid.clone();
                let load_result = tokio::task::spawn_blocking(move || {
                    let db = Database::open_default()?;
                    let store = SessionStore::new(db);
                    store.load_messages(&sid_for_load)
                })
                .await;

                match load_result {
                    Ok(Ok(messages)) => app.restore_history(messages),
                    Ok(Err(err)) => {
                        tracing::warn!(session_id = %sid, "failed to load session history: {err}");
                    }
                    Err(err) => {
                        tracing::warn!("failed to join session history load task: {err}");
                    }
                }
            }

            app.set_workspace_dir(workspace_root.to_string_lossy().into_owned());

            match spawn_agent_bridge(workspace_root, session_id) {
                Ok((event_rx, cmd_tx)) => {
                    app.set_bridge(event_rx, cmd_tx);
                }
                Err(err) => {
                    tracing::warn!("failed to spawn agent bridge: {err}");
                    app.thread
                        .messages
                        .push(crate::thread::ThreadMessage::System {
                            content: format!("⚠ Agent bridge failed: {err}"),
                            timestamp: chrono::Utc::now(),
                        });
                }
            }

            let mut events = EventStream::new();
            let mut tick_interval = tokio::time::interval(Duration::from_millis(60));

            loop {
                app.drain_agent_events();
                terminal.draw(|f| app.draw(f))?;

                tokio::select! {
                    Some(event_result) = events.next() => {
                        match event_result {
                            Ok(event) => {
                                app.handle_event(event).await?;
                            }
                            Err(e) => {
                                tracing::warn!("event stream error: {}", e);
                            }
                        }
                    }
                    _ = tick_interval.tick() => {
                        app.tick_count = app.tick_count.wrapping_add(1);
                        app.dispatch_console.on_tick();
                    }
                }

                if app.state == app::AppState::Quitting {
                    break;
                }
            }

            ratatui::restore();

            Ok(())
        })
        .await
}
