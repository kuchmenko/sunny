//! Interactive REPL chat command (`sunny chat`).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use crossterm::style::Stylize;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use sunny_mind::{AnthropicProvider, LlmProvider, StreamEvent};
use sunny_store::{Database, SessionStore};

use crate::chat::ChatSession;

/// Arguments for the `sunny chat` subcommand.
#[derive(Args, Debug)]
pub struct ChatArgs {
    /// LLM model to use.
    #[arg(long, default_value = "claude-sonnet-4-6")]
    pub model: String,

    /// Anthropic API key override (overrides ANTHROPIC_API_KEY env var and OAuth credentials).
    #[arg(long)]
    pub api_key: Option<String>,
}

/// Run the interactive chat REPL.
pub async fn run(args: ChatArgs) -> anyhow::Result<()> {
    // Detect workspace root: prefer git root, fall back to cwd.
    let workspace_root = detect_workspace_root();

    // Set ANTHROPIC_API_KEY if an explicit override was provided.
    if let Some(ref key) = args.api_key {
        // SAFETY: single-threaded at startup; tokio runtime not yet handling IO.
        // This is an intentional environment override for the current process only.
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", key);
        }
    }

    // Determine auth source label for the welcome banner.
    let auth_source = detect_auth_source(&args.api_key);

    // Construct the Anthropic provider (auto-detects auth: env var → credential file).
    let provider: Arc<dyn LlmProvider> = Arc::new(
        AnthropicProvider::new(&args.model)
            .map_err(|e| anyhow::anyhow!("failed to initialise Anthropic provider: {e}"))?,
    );

    // Print welcome banner.
    println!(
        "╭─────────────────────────────────────────╮\n\
         │  sunny chat                             │\n\
         │  Model:     {model:<29}│\n\
         │  Auth:      {auth:<29}│\n\
         │  Workspace: {ws:<29}│\n\
         ╰─────────────────────────────────────────╯",
        model = truncate(&args.model, 29),
        auth = truncate(&auth_source, 29),
        ws = truncate(&workspace_root.display().to_string(), 29),
    );
    println!("Type /quit or /exit to leave. Ctrl+C cancels the current request.\n");

    let store_db = Database::open_default()
        .map_err(|e| anyhow::anyhow!("failed to open session store: {e}"))?;
    #[allow(clippy::arc_with_non_send_sync)]
    let store = Arc::new(SessionStore::new(store_db));
    let session_id = format!(
        "pending-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let mut session = ChatSession::new(
        Arc::clone(&provider),
        workspace_root.clone(),
        session_id,
        store,
    );
    let mut rl = DefaultEditor::new()?;

    loop {
        let readline = rl.readline("> ");
        match readline {
            Ok(line) => {
                let input = line.trim().to_string();
                if input.is_empty() {
                    continue;
                }
                if input == "/quit" || input == "/exit" {
                    println!("Goodbye.");
                    break;
                }

                let _ = rl.add_history_entry(&input);

                // Run the streaming request.
                let result = session
                    .send(&input, |event| {
                        handle_stream_event(event);
                    })
                    .await;

                // Newline after streamed output.
                println!();

                if let Err(e) = result {
                    eprintln!("Error: {e}");
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C — cancel current request (if running), continue REPL.
                session.cancel_current();
                eprintln!("\n[cancelled]");
            }
            Err(ReadlineError::Eof) => {
                // Ctrl+D — exit.
                println!("Goodbye.");
                break;
            }
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
    }

    Ok(())
}

/// Handle a single stream event from the tool loop.
fn handle_stream_event(event: StreamEvent) {
    match event {
        StreamEvent::ContentDelta { text } => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        StreamEvent::ToolCallStart { name, .. } => {
            eprintln!("{}", format!("[tool: {name}]").grey());
        }
        _ => {}
    }
}

/// Detect the workspace root by running `git rev-parse --show-toplevel`.
/// Falls back to the current directory if git is unavailable or fails.
fn detect_workspace_root() -> PathBuf {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout)
                    .ok()
                    .map(|s| PathBuf::from(s.trim()))
            } else {
                None
            }
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Determine the auth source label for the welcome banner.
fn detect_auth_source(api_key_override: &Option<String>) -> String {
    if api_key_override.is_some() {
        return "api-key (--api-key flag)".to_string();
    }
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        return "api-key (ANTHROPIC_API_KEY)".to_string();
    }
    let cred_path = dirs::home_dir()
        .map(|h| h.join(".claude").join(".credentials.json"))
        .filter(|p| p.exists());
    if cred_path.is_some() {
        return "oauth (~/.claude/.credentials.json)".to_string();
    }
    "unknown".to_string()
}

/// Truncate a string to fit within `max_len` display chars (ASCII-safe).
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len.saturating_sub(1)])
    }
}
