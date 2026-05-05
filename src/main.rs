mod credentials;
mod openai;

use std::{io::Write, sync::Arc};

use anyhow::Result;
use futures::StreamExt;
use tkach::{tools, Agent, CancellationToken, Content, Message};
use tokio::io::{self, AsyncBufReadExt, BufReader};

use crate::{credentials::CredentialsManager, openai::OpenAICodex};

#[derive(Default, Debug)]
struct State {
    is_authenticating: bool,
    is_authenticated: bool,
    client: Arc<reqwest::Client>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv()?;

    let stdin = io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();
    let mut state = State::default();
    let client = reqwest::Client::new();
    state.client = Arc::new(client);

    let creds_manager = CredentialsManager::new()?;
    let mut agent = creds_manager
        .get_openai()?
        .map(|credentials| create_openai_agent(credentials, creds_manager.clone()));
    state.is_authenticated = agent.is_some();

    if state.is_authenticated {
        println!("Loaded OpenAI subscription credentials.");
    } else {
        println!("Not authenticated. Run .login first.");
    }

    let mut messages = Vec::new();

    loop {
        while let Some(line) = lines.next_line().await? {
            let is_command = detect_command(&line, &mut state, &mut agent, &creds_manager).await?;

            if is_command || state.is_authenticating {
                continue;
            }

            let Some(agent) = agent.as_ref() else {
                println!("Not authenticated. Run .login first.");
                continue;
            };

            handle_agent_message(agent, &line, &mut messages).await?;
        }
    }
}

fn create_openai_agent(
    credentials: openai::OAuthCredentials,
    creds_manager: CredentialsManager,
) -> Agent {
    Agent::builder()
        .provider(OpenAICodex::new(credentials, creds_manager))
        .model("gpt-5.5")
        .system("You are a concise assistant.")
        .tools(tools::defaults())
        .build()
}

async fn handle_agent_message(
    agent: &Agent,
    line: &str,
    messages: &mut Vec<Message>,
) -> Result<()> {
    messages.push(Message::user(vec![Content::text(line)]));
    let resp = process_input(agent, messages).await?;

    messages.push(Message::assistant(vec![Content::text(resp)]));

    Ok(())
}

async fn process_input(agent: &Agent, messages: &[Message]) -> Result<String> {
    let token = CancellationToken::new();
    let mut stream = agent.stream(messages.to_vec(), token);
    let mut content = String::new();

    while let Some(event) = stream.next().await {
        match event? {
            tkach::StreamEvent::ContentDelta(cd) => {
                print!("{}", cd);
                std::io::stdout().flush()?;
                content.push_str(&cd);
            }
            tkach::StreamEvent::ToolUse { id, name, input } => {
                println!("tool_use: {}, {}, {}", id, name, input);
            }
            tkach::StreamEvent::ToolCallPending {
                id,
                name,
                input,
                class,
            } => {
                println!(
                    "tool_call_pending: {}, {}, {}, {:?}",
                    id, name, input, class
                );

                let _ = class;
            }
            tkach::StreamEvent::MessageDelta { stop_reason } => {
                println!("message_delta: {:?}", stop_reason);
            }
            tkach::StreamEvent::Usage(usage) => {
                println!("usage: {:?}", usage);
            }
            tkach::StreamEvent::Done => {
                println!("done");
            }
        }
    }

    let result = stream.into_result().await?;
    let text = if content.is_empty() {
        result.text
    } else {
        content
    };

    if !text.is_empty() {
        println!();
    }

    Ok(text)
}

async fn detect_command(
    line: &str,
    state: &mut State,
    agent: &mut Option<Agent>,
    creds_manager: &CredentialsManager,
) -> Result<bool> {
    match (state.is_authenticating, line) {
        (true, "1") => {
            println!("start openai oauth");
            let credentials = openai::run_oauth_flow(&state.client).await?;
            creds_manager.set_openai(credentials.clone())?;
            *agent = Some(create_openai_agent(credentials, creds_manager.clone()));
            state.is_authenticating = false;
            state.is_authenticated = true;

            return Ok(true);
        }
        (true, "2") => {
            println!("start anthropic oauth");
            return Ok(true);
        }
        _ => {}
    }

    match line {
        ".exit" => {
            std::process::exit(0);
        }
        ".login" => {
            state.is_authenticating = true;
            state.is_authenticated = false;

            println!("Select subscription provider: 1 - OpenAI, 2 - Anthropic");
            return Ok(true);
        }
        _ => {}
    };

    Ok(false)
}
