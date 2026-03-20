//! Live smoke test: binary-searches system prompt length to find the 400 threshold.
//!
//! Usage:
//!   cargo run -p sunny-boys --example smoke_session

use std::path::PathBuf;
use std::sync::Arc;

use sunny_boys::{AgentSession, AlwaysAllowGate, SharedApprovalGate};
use sunny_mind::{
    AnthropicProvider, ChatMessage, ChatRole, LlmProvider, LlmRequest, StreamEvent,
};
use sunny_store::{Database, SessionStore};
use tokio_stream::StreamExt;

async fn try_system(provider: &Arc<dyn LlmProvider>, system: &str) -> bool {
    let req = LlmRequest {
        messages: vec![
            ChatMessage {
                role: ChatRole::System,
                content: system.to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::User,
                content: "hey".into(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ],
        max_tokens: Some(64),
        temperature: None,
        tools: None,
        tool_choice: None,
        thinking_budget: None,
    };

    match provider.chat_stream(req).await {
        Err(_) => false,
        Ok(mut stream) => {
            while let Some(event) = stream.next().await {
                match event {
                    Err(_) => return false,
                    Ok(StreamEvent::Done) => break,
                    _ => {}
                }
            }
            true
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let provider = Arc::new(AnthropicProvider::new("claude-sonnet-4-6")?) as Arc<dyn LlmProvider>;
    let db = Database::open_default()?;
    let store = Arc::new(SessionStore::new(db));
    let approval = Arc::new(AlwaysAllowGate) as SharedApprovalGate;

    let tmp = AgentSession::new(Arc::clone(&provider), workspace_root, "tmp".into(), Arc::clone(&store));
    let full_system = tmp.messages()[0].content.clone();
    eprintln!("full system_len = {}", full_system.len());

    // Binary search: find shortest prefix that causes the 400
    let mut lo = 0usize;
    let mut hi = full_system.len();

    // Quick sanity check: just the Claude Code identity line (no extra content)
    let minimal = "You are Claude Code, Anthropic's official CLI for Claude.";
    let minimal_ok = try_system(&provider, minimal).await;
    eprintln!("minimal system ({} chars): {}", minimal.len(), if minimal_ok { "OK" } else { "FAIL" });

    if minimal_ok {
        // Binary search for the breaking point
        eprintln!("Binary searching for break point...");
        while hi - lo > 100 {
            let mid = (lo + hi) / 2;
            let prefix: String = full_system.chars().take(mid).collect();
            let ok = try_system(&provider, &prefix).await;
            eprintln!("  len={}: {}", prefix.len(), if ok { "OK" } else { "FAIL" });
            if ok { lo = mid; } else { hi = mid; }
        }
        eprintln!("\nBreaking point around char {}-{}", lo, hi);

        // Show the content around the break point
        let before: String = full_system.chars().take(lo).collect();
        let breaking: String = full_system.chars().take(hi).collect();
        eprintln!("\n--- content at breakpoint (chars {}..{}) ---", lo, hi);
        eprintln!("{}", &breaking[lo.min(breaking.len())..]);
        eprintln!("---");
        let _ = (before, approval);
    } else {
        eprintln!("ERROR: Even minimal system prompt fails — not a content issue");
    }

    Ok(())
}
