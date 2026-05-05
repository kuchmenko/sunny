mod credentials;
mod openai;

use std::{
    io::{IsTerminal, Write},
    sync::Arc,
};

use anyhow::Result;
use futures::StreamExt;
use tkach::{tools, Agent, CancellationToken, Content, Message};
use tokio::io::{self as tokio_io, AsyncBufReadExt, BufReader};

use crate::{credentials::CredentialsManager, openai::OpenAICodex};

const RESET: &str = "\x1b[0m";
const SUN: &str = "1;38;5;214";
const CREAM: &str = "38;5;230";
const ORANGE: &str = "38;5;208";
const RED: &str = "1;38;5;196";
const GREEN: &str = "38;5;150";
const DIM: &str = "2;38;5;244";
const FRAME: &str = "38;5;240";

#[derive(Default, Debug)]
struct State {
    is_authenticating: bool,
    is_authenticated: bool,
    client: Arc<reqwest::Client>,
}

#[derive(Clone, Copy)]
struct Ui {
    color: bool,
    interactive: bool,
}

impl Ui {
    fn detect() -> Self {
        let stdin_tty = std::io::stdin().is_terminal();
        let stdout_tty = std::io::stdout().is_terminal();

        Self {
            color: stdout_tty && std::env::var_os("NO_COLOR").is_none(),
            interactive: stdin_tty && stdout_tty,
        }
    }

    fn paint(&self, code: &str, text: impl AsRef<str>) -> String {
        let text = text.as_ref();
        if self.color {
            format!("\x1b[{code}m{text}{RESET}")
        } else {
            text.to_owned()
        }
    }

    fn prompt(&self, state: &State) -> String {
        if state.is_authenticating {
            return format!("{} {} ", self.paint(ORANGE, "auth"), self.paint(RED, "›"));
        }

        format!("{} {} ", self.paint(SUN, "☀ sunny"), self.paint(RED, "›"))
    }

    fn assistant_prefix(&self) -> String {
        format!("{} ", self.paint(SUN, "☀"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv()?;

    let ui = Ui::detect();
    let stdin = tokio_io::stdin();
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

    if ui.interactive {
        print_banner(&ui, state.is_authenticated);
    }

    let mut messages = Vec::new();

    loop {
        print_prompt(&ui, &state)?;

        let Some(line) = lines.next_line().await? else {
            if ui.interactive {
                println!();
            }
            break;
        };

        if line.trim().is_empty() {
            continue;
        }

        let is_command = detect_command(&line, &ui, &mut state, &mut agent, &creds_manager).await?;

        if is_command || state.is_authenticating {
            continue;
        }

        let Some(agent) = agent.as_ref() else {
            print_error(&ui, "not authenticated; run .login first");
            continue;
        };

        if let Err(err) = handle_agent_message(agent, &line, &ui, &mut messages).await {
            print_error(&ui, err.to_string());
        }
    }

    Ok(())
}

fn create_openai_agent(
    credentials: openai::OAuthCredentials,
    creds_manager: CredentialsManager,
) -> Agent {
    Agent::builder()
        .provider(OpenAICodex::new(credentials, creds_manager))
        .model("gpt-5.5")
        .system("You are Sunny, a concise terminal assistant. Be direct and useful.")
        .tools(tools::defaults())
        .build()
}

async fn handle_agent_message(
    agent: &Agent,
    line: &str,
    ui: &Ui,
    messages: &mut Vec<Message>,
) -> Result<()> {
    messages.push(Message::user(vec![Content::text(line)]));

    match process_input(agent, messages, ui).await {
        Ok(resp) => {
            if !resp.is_empty() {
                messages.push(Message::assistant(vec![Content::text(resp)]));
            }
            Ok(())
        }
        Err(err) => {
            messages.pop();
            Err(err)
        }
    }
}

async fn process_input(agent: &Agent, messages: &[Message], ui: &Ui) -> Result<String> {
    let token = CancellationToken::new();
    let mut stream = agent.stream(messages.to_vec(), token);
    let mut content = String::new();
    let mut assistant_started = false;
    let debug = stream_debug_enabled();

    while let Some(event) = stream.next().await {
        match event? {
            tkach::StreamEvent::ContentDelta(delta) => {
                if !assistant_started {
                    print!("{}", ui.assistant_prefix());
                    std::io::stdout().flush()?;
                    assistant_started = true;
                }

                print!("{delta}");
                std::io::stdout().flush()?;
                content.push_str(&delta);
            }
            tkach::StreamEvent::ToolUse { id, name, input } => {
                if debug {
                    print_debug(ui, format!("tool_use {id} {name} {input}"));
                }
            }
            tkach::StreamEvent::ToolCallPending {
                id,
                name,
                input,
                class,
            } => {
                if debug {
                    print_debug(ui, format!("tool_pending {id} {name} {input} {class:?}"));
                }
            }
            tkach::StreamEvent::MessageDelta { stop_reason } => {
                if debug {
                    print_debug(ui, format!("stop_reason {stop_reason:?}"));
                }
            }
            tkach::StreamEvent::Usage(usage) => {
                if debug {
                    print_debug(ui, format!("usage {usage:?}"));
                }
            }
            tkach::StreamEvent::Done => {
                if debug {
                    print_debug(ui, "done");
                }
            }
        }
    }

    let streamed = !content.is_empty();
    let result = stream.into_result().await?;

    if !streamed && !result.text.is_empty() {
        print!("{}{}", ui.assistant_prefix(), result.text);
        std::io::stdout().flush()?;
    }

    let text = if streamed { content } else { result.text };
    if !text.is_empty() {
        println!();
    }

    Ok(text)
}

async fn detect_command(
    line: &str,
    ui: &Ui,
    state: &mut State,
    agent: &mut Option<Agent>,
    creds_manager: &CredentialsManager,
) -> Result<bool> {
    let command = line.trim();

    if command == ".exit" || command == ".quit" {
        println!("{}", ui.paint(DIM, "bye"));
        std::process::exit(0);
    }

    if state.is_authenticating {
        match command {
            "1" => {
                print_note(ui, "opening OpenAI OAuth in your browser");
                let credentials = openai::run_oauth_flow(&state.client).await?;
                creds_manager.set_openai(credentials.clone())?;
                *agent = Some(create_openai_agent(credentials, creds_manager.clone()));
                state.is_authenticating = false;
                state.is_authenticated = true;
                print_success(ui, "OpenAI subscription credentials saved");
                return Ok(true);
            }
            "2" => {
                state.is_authenticating = false;
                state.is_authenticated = agent.is_some();
                print_error(ui, "Anthropic OAuth is not implemented yet");
                return Ok(true);
            }
            ".cancel" => {
                state.is_authenticating = false;
                state.is_authenticated = agent.is_some();
                print_note(ui, "authentication cancelled");
                return Ok(true);
            }
            ".help" => {
                print_help(ui);
                return Ok(true);
            }
            _ => {
                print_error(ui, "choose 1 for OpenAI, 2 for Anthropic, or .cancel");
                return Ok(true);
            }
        }
    }

    match command {
        ".help" => {
            print_help(ui);
            Ok(true)
        }
        ".login" => {
            state.is_authenticating = true;
            print_login_choices(ui);
            Ok(true)
        }
        ".status" => {
            print_auth_status(ui, state.is_authenticated);
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn print_prompt(ui: &Ui, state: &State) -> Result<()> {
    if ui.interactive {
        print!("{}", ui.prompt(state));
        std::io::stdout().flush()?;
    }

    Ok(())
}

fn print_banner(ui: &Ui, authenticated: bool) {
    println!(
        "{}",
        ui.paint(FRAME, "╭────────────────────────────────────────────╮")
    );
    println!(
        "{}  {}  {}",
        ui.paint(FRAME, "│"),
        ui.paint(SUN, "☀ SUNNY"),
        ui.paint(DIM, "retro subscription console")
    );
    println!(
        "{}",
        ui.paint(FRAME, "╰────────────────────────────────────────────╯")
    );
    print_auth_status(ui, authenticated);
    println!(
        "{} {}",
        ui.paint(DIM, "hint:"),
        ui.paint(CREAM, ".help shows commands")
    );
}

fn print_help(ui: &Ui) {
    println!("{}", ui.paint(SUN, "commands"));
    println!(
        "  {}  {}",
        ui.paint(CREAM, ".login"),
        ui.paint(DIM, "connect subscription credentials")
    );
    println!(
        "  {} {}",
        ui.paint(CREAM, ".status"),
        ui.paint(DIM, "show auth state")
    );
    println!(
        "  {}   {}",
        ui.paint(CREAM, ".exit"),
        ui.paint(DIM, "leave the REPL")
    );
    println!(
        "  {}  {}",
        ui.paint(CREAM, ".cancel"),
        ui.paint(DIM, "cancel authentication prompt")
    );
    println!(
        "  {}",
        ui.paint(DIM, "set SUNNY_DEBUG_STREAM=1 to show raw stream events")
    );
}

fn print_login_choices(ui: &Ui) {
    println!("{}", ui.paint(SUN, "select subscription provider"));
    println!("  {} {}", ui.paint(CREAM, "1"), ui.paint(DIM, "OpenAI"));
    println!(
        "  {} {}",
        ui.paint(CREAM, "2"),
        ui.paint(DIM, "Anthropic — not implemented")
    );
    println!("  {}", ui.paint(DIM, ".cancel to return"));
}

fn print_auth_status(ui: &Ui, authenticated: bool) {
    let status = if authenticated {
        ui.paint(GREEN, "OpenAI credentials loaded")
    } else {
        ui.paint(RED, "not authenticated; run .login")
    };

    println!("{} {}", ui.paint(DIM, "auth:"), status);
}

fn print_success(ui: &Ui, message: impl AsRef<str>) {
    println!("{} {}", ui.paint(GREEN, "✓"), message.as_ref());
}

fn print_error(ui: &Ui, message: impl AsRef<str>) {
    println!("{} {}", ui.paint(RED, "✕"), message.as_ref());
}

fn print_note(ui: &Ui, message: impl AsRef<str>) {
    println!("{} {}", ui.paint(ORANGE, "›"), message.as_ref());
}

fn print_debug(ui: &Ui, message: impl AsRef<str>) {
    eprintln!("{} {}", ui.paint(DIM, "debug:"), message.as_ref());
}

fn stream_debug_enabled() -> bool {
    matches!(
        std::env::var("SUNNY_DEBUG_STREAM").as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}
