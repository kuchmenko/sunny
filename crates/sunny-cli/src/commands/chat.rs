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
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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

    /// Resume the most recent session for the current working directory.
    #[arg(long = "continue")]
    pub continue_session: bool,

    /// Resume a specific session by ID.
    #[arg(long)]
    pub resume: Option<String>,
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
    let mut session = if args.continue_session || args.resume.is_some() {
        let cwd = workspace_root.to_string_lossy().to_string();
        let resume_id = args.resume.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            let db = Database::open_default()
                .map_err(|e| anyhow::anyhow!("failed to open session store: {e}"))?;
            let store = SessionStore::new(db);

            let saved = if let Some(id) = resume_id {
                let exact = store
                    .load_session(&id)
                    .map_err(|e| anyhow::anyhow!("failed to load session: {e}"))?;
                if exact.is_some() {
                    exact
                } else {
                    let matches = store
                        .search_sessions(&id)
                        .map_err(|e| anyhow::anyhow!("failed to search sessions: {e}"))?;
                    matches.into_iter().next()
                }
            } else {
                store
                    .most_recent_session(&cwd)
                    .map_err(|e| anyhow::anyhow!("failed to find most recent session: {e}"))?
            };

            match saved {
                Some(saved_session) => {
                    let messages = store
                        .load_messages(&saved_session.id)
                        .map_err(|e| anyhow::anyhow!("failed to load messages: {e}"))?;
                    Ok::<_, anyhow::Error>(Some((saved_session, messages)))
                }
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("failed to join session load task: {e}"))??;

        if let Some((saved_session, messages)) = loaded {
            println!(
                "Resumed session: {} ({}) - {} messages",
                saved_session.title.as_deref().unwrap_or("untitled"),
                saved_session.id,
                messages.len()
            );
            ChatSession::from_saved(
                Arc::clone(&store),
                saved_session,
                messages,
                Arc::clone(&provider),
                workspace_root.clone(),
                CancellationToken::new(),
            )
        } else {
            eprintln!("No session found. Starting new session.");
            create_new_session(
                Arc::clone(&provider),
                workspace_root.clone(),
                Arc::clone(&store),
            )
        }
    } else {
        create_new_session(
            Arc::clone(&provider),
            workspace_root.clone(),
            Arc::clone(&store),
        )
    };
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

                if input.starts_with('/') {
                    let mut parts = input.split_whitespace();
                    let command = parts.next().unwrap_or_default();

                    match command {
                        "/sessions" => {
                            let sub = parts.next();
                            if sub.is_none() || sub == Some("list") {
                                let cwd = workspace_root.to_string_lossy().to_string();
                                let listed = tokio::task::spawn_blocking(move || {
                                    let db = Database::open_default().map_err(|e| {
                                        anyhow::anyhow!("failed to open session store: {e}")
                                    })?;
                                    let store = SessionStore::new(db);
                                    store.list_sessions(Some(&cwd)).map_err(|e| {
                                        anyhow::anyhow!("failed to list sessions: {e}")
                                    })
                                })
                                .await
                                .map_err(|e| {
                                    anyhow::anyhow!("failed to join session list task: {e}")
                                })?;

                                match listed {
                                    Ok(sessions) if sessions.is_empty() => {
                                        println!("No sessions found.")
                                    }
                                    Ok(sessions) => {
                                        println!(
                                            "{:<36} {:<25} {:>5}  Updated",
                                            "ID", "Title", "Msgs"
                                        );
                                        println!("{}", "-".repeat(80));
                                        for s in sessions {
                                            let title: String = s
                                                .title
                                                .as_deref()
                                                .unwrap_or("untitled")
                                                .chars()
                                                .take(25)
                                                .collect();
                                            println!(
                                                "{:<36} {:<25} {:>5}  {}",
                                                &s.id[..36.min(s.id.len())],
                                                title,
                                                s.token_count,
                                                s.updated_at.format("%Y-%m-%d %H:%M")
                                            );
                                        }
                                    }
                                    Err(e) => eprintln!("Failed to list sessions: {e}"),
                                }
                            } else {
                                eprintln!("Usage: /sessions list");
                            }
                        }
                        "/new" => {
                            println!("Starting new session...");
                            session = create_new_session(
                                Arc::clone(&provider),
                                workspace_root.clone(),
                                Arc::clone(&store),
                            );
                        }
                        "/clear" => {
                            println!("Cleared session messages.");
                            session = ChatSession::new(
                                Arc::clone(&provider),
                                workspace_root.clone(),
                                session.session_id().to_string(),
                                Arc::clone(&store),
                            );
                        }
                        "/switch" => {
                            if let Some(id) = parts.next() {
                                let id = id.to_string();
                                let loaded = tokio::task::spawn_blocking(move || {
                                    let db = Database::open_default().map_err(|e| {
                                        anyhow::anyhow!("failed to open session store: {e}")
                                    })?;
                                    let store = SessionStore::new(db);
                                    let saved = store.load_session(&id).map_err(|e| {
                                        anyhow::anyhow!("failed to load session: {e}")
                                    })?;
                                    match saved {
                                        Some(saved_session) => {
                                            let messages = store
                                                .load_messages(&saved_session.id)
                                                .map_err(|e| {
                                                anyhow::anyhow!(
                                                    "failed to load session messages: {e}"
                                                )
                                            })?;
                                            Ok::<_, anyhow::Error>(Some((saved_session, messages)))
                                        }
                                        None => Ok(None),
                                    }
                                })
                                .await
                                .map_err(|e| {
                                    anyhow::anyhow!("failed to join session switch task: {e}")
                                })?;

                                match loaded {
                                    Ok(Some((saved_session, messages))) => {
                                        println!("Switched to session: {}", saved_session.id);
                                        session = ChatSession::from_saved(
                                            Arc::clone(&store),
                                            saved_session,
                                            messages,
                                            Arc::clone(&provider),
                                            workspace_root.clone(),
                                            CancellationToken::new(),
                                        );
                                    }
                                    Ok(None) => eprintln!("Session not found."),
                                    Err(e) => eprintln!("Failed to switch sessions: {e}"),
                                }
                            } else {
                                eprintln!("Usage: /switch <id>");
                            }
                        }
                        "/compact" | "/compact " => {
                            println!("Compacting conversation...");
                            match session.compact_with_llm().await {
                                Ok(msg) => println!("{msg}"),
                                Err(e) => eprintln!("Compaction failed: {e}"),
                            }
                        }
                        "/reindex" => {
                            println!("Indexing codebase...");
                            let root = workspace_root.clone();
                            match tokio::task::spawn_blocking(move || {
                                let db = sunny_store::Database::open_default()
                                    .map_err(|e| anyhow::anyhow!("failed to open index db: {e}"))?;
                                let idx = sunny_store::SymbolIndex::new(db);
                                idx.index_directory(&root)
                                    .map_err(|e| anyhow::anyhow!("indexing failed: {e}"))
                            })
                            .await
                            {
                                Ok(Ok(count)) => println!("Indexed {count} symbols."),
                                Ok(Err(e)) => eprintln!("Reindex failed: {e}"),
                                Err(e) => eprintln!("Reindex task panicked: {e}"),
                            }
                        }
                        "/help" => {
                            println!("Available commands:");
                            println!(
                                "  /sessions list         List sessions for current directory"
                            );
                            println!("  /new                   Start a new session");
                            println!("  /clear                 Clear current session messages");
                            println!("  /switch <id>           Switch to a specific session");
                            println!("  /compact               Compact conversation context");
                            println!(
                                "  /reindex               Index the codebase for symbol search"
                            );
                            println!("  /help                  Show this help");
                            println!("  /quit, /exit           Exit sunny");
                        }
                        _ => eprintln!("Unknown command. Type /help for available commands."),
                    }

                    continue;
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

#[allow(clippy::arc_with_non_send_sync)]
fn create_new_session(
    provider: Arc<dyn LlmProvider>,
    workspace_root: PathBuf,
    store: Arc<SessionStore>,
) -> ChatSession {
    let session_id = Uuid::new_v4().to_string();

    ChatSession::new(provider, workspace_root, session_id, store)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::ChatArgs;

    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(flatten)]
        args: ChatArgs,
    }

    #[test]
    fn test_parse_continue_flag() {
        let parsed = TestCli::parse_from(["sunny", "--continue"]);
        assert!(parsed.args.continue_session);
    }

    #[test]
    fn test_parse_resume_flag() {
        let parsed = TestCli::parse_from(["sunny", "--resume", "session-123"]);
        assert_eq!(parsed.args.resume.as_deref(), Some("session-123"));
    }
}
