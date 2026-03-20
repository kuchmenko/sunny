//! Live smoke test: sends "hey" via the real AnthropicProvider and prints the reply.
//!
//! Usage:
//!   cargo run -p sunny-mind --example smoke_hey
//!   cargo run -p sunny-mind --example smoke_hey -- --tools   # simulate TUI (sonnet + tools)
//!   RUST_LOG=trace cargo run -p sunny-mind --example smoke_hey 2>&1 | grep -E 'anthropic-beta|400|401'

use serde_json::json;
use sunny_mind::{
    AnthropicProvider, ChatMessage, ChatRole, LlmProvider, LlmRequest, StreamEvent, ToolChoice,
    ToolDefinition,
};
use tokio_stream::StreamExt;

fn minimal_tool() -> ToolDefinition {
    ToolDefinition {
        name: "fs_read".to_string(),
        description: "Read a file from the filesystem".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file" }
            },
            "required": ["path"]
        }),
        group: Default::default(),
        hint: None,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let with_tools = std::env::args().any(|a| a == "--tools");
    let model = if with_tools {
        "claude-sonnet-4-6"
    } else {
        "claude-haiku-4-5-20251001"
    };

    println!("Model: {model}  tools: {with_tools}");

    let provider = AnthropicProvider::new(model)?;

    let request = LlmRequest {
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: "hey".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        max_tokens: Some(64),
        temperature: Some(0.0),
        tools: if with_tools {
            Some(vec![minimal_tool()])
        } else {
            None
        },
        tool_choice: if with_tools {
            Some(ToolChoice::Auto)
        } else {
            None
        },
        thinking_budget: None,
    };

    print!("Response: ");

    let mut stream = provider.chat_stream(request).await?;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::ContentDelta { text } => print!("{text}"),
            StreamEvent::Usage { usage } => {
                println!();
                println!(
                    "[tokens — in:{} out:{} total:{}]",
                    usage.input_tokens, usage.output_tokens, usage.total_tokens
                );
            }
            StreamEvent::Done => {}
            StreamEvent::Error { message } => eprintln!("\n[stream error: {message}]"),
            _ => {}
        }
    }

    Ok(())
}
