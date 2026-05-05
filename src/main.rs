mod credentials;
mod openai;

use std::{
    collections::BTreeMap,
    io::{IsTerminal, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use tkach::{
    tools, Agent, AgentResult, CancellationToken, Content, Message, StreamEvent, Tool, ToolClass,
    ToolContext, ToolError, ToolOutput,
};
use tokio::{
    io::{self as tokio_io, AsyncBufReadExt, BufReader},
    sync::broadcast,
};

use crate::{credentials::CredentialsManager, openai::OpenAICodex};

const RESET: &str = "\x1b[0m";
const SUN: &str = "1;38;5;214";
const CREAM: &str = "38;5;230";
const ORANGE: &str = "38;5;208";
const RED: &str = "1;38;5;196";
const GREEN: &str = "38;5;150";
const DIM: &str = "38;5;250";
const FRAME: &str = "38;5;245";
const MAX_TOOL_OUTPUT_CHARS: usize = 16_000;
const SYSTEM_PROMPT: &str = r#"You are Sunny, a concise terminal assistant. Be direct and useful.

Command-following contract:
- Treat imperative user messages as tasks to execute, not topics to discuss.
- If the user asks to use tools, use tools unless impossible.
- For tasks about the environment, files, repo, codebase, shell state, or tool availability, verify with tools before answering.
- Never claim tools or filesystem access are unavailable when tools are available.
- Refer to tools by their Sunny names: bash, read, write, edit, grep, glob.
- If required details are missing, choose the safest useful default instead of asking.
- Ask only when ambiguity changes risk, writes data, touches secrets/auth, or blocks all progress.
- If a target is broad, inspect shallowly first, then narrow.
- If a target is a directory but the requested tool reads files, list the directory first.
- Prefer specialized tools over bash: glob for file discovery, grep for code search, read for file contents.
- Use bash only as an integration escape hatch: build/test commands, git state, external CLIs, process/env checks, or tasks specialized tools cannot express.
- Do not use bash for basic listing/search/reading when glob/grep/read can satisfy the task.
- If bash is necessary, keep commands short, bounded, and read-only unless the user requested mutation.
- If a tool fails because scope or output is too large, retry once with a smaller bounded scope.
- Prefer bounded commands and reads: shallow listings, targeted files, explicit limits.
- Summarize findings; do not dump large raw outputs unless asked."#;

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
    let tool_reporter = ToolReporter::new();
    let mut agent = creds_manager.get_openai()?.map(|credentials| {
        create_openai_agent(credentials, creds_manager.clone(), tool_reporter.clone())
    });
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

        let is_command = detect_command(
            &line,
            &ui,
            &mut state,
            &mut agent,
            &creds_manager,
            &tool_reporter,
        )
        .await?;

        if is_command || state.is_authenticating {
            continue;
        }

        let Some(agent) = agent.as_ref() else {
            print_error(&ui, "not authenticated; run .login first");
            continue;
        };

        if let Err(err) =
            handle_agent_message(agent, &line, &ui, &tool_reporter, &mut messages).await
        {
            print_error(&ui, err.to_string());
        }
    }

    Ok(())
}

fn create_openai_agent(
    credentials: openai::OAuthCredentials,
    creds_manager: CredentialsManager,
    tool_reporter: ToolReporter,
) -> Agent {
    Agent::builder()
        .provider(OpenAICodex::new(credentials, creds_manager))
        .model("gpt-5.5")
        .system(SYSTEM_PROMPT)
        .tools(bounded_default_tools(tool_reporter))
        .build()
}

#[derive(Clone)]
struct ToolReporter {
    tx: broadcast::Sender<ToolRunEvent>,
    next_id: Arc<AtomicU64>,
}

impl ToolReporter {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(512);

        Self {
            tx,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<ToolRunEvent> {
        self.tx.subscribe()
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, event: ToolRunEvent) {
        let _ = self.tx.send(event);
    }
}

#[derive(Clone)]
enum ToolRunEvent {
    Started {
        id: u64,
        name: String,
        input: Value,
        class: ToolClass,
    },
    Finished {
        id: u64,
        name: String,
        class: ToolClass,
        duration: Duration,
        output_chars: usize,
        sent_chars: usize,
        is_error: bool,
        preview: String,
    },
}

struct BoundedTool {
    inner: Arc<dyn Tool>,
    description: String,
    reporter: ToolReporter,
}

impl BoundedTool {
    fn new(inner: Arc<dyn Tool>, reporter: ToolReporter) -> Self {
        Self {
            description: bounded_tool_description(inner.as_ref()),
            inner,
            reporter,
        }
    }
}

fn bounded_tool_description(tool: &dyn Tool) -> String {
    let guidance = match tool.name() {
        "bash" => "Use only as an integration escape hatch for build/test commands, git state, external CLIs, process/env checks, or tasks specialized tools cannot express. Prefer glob/read/grep for filesystem and code inspection.",
        "glob" => "Preferred tool for file discovery and directory-style inspection. Use before bash for listing project files.",
        "grep" => "Preferred tool for searching code or text. Use before bash grep/rg/sed pipelines.",
        "read" => "Preferred tool for reading file contents. Use before bash cat/sed/head/tail.",
        _ => "",
    };

    if guidance.is_empty() {
        format!(
            "{} Return bounded output; large results are truncated by Sunny.",
            tool.description()
        )
    } else {
        format!(
            "{} {guidance} Return bounded output; large results are truncated by Sunny.",
            tool.description()
        )
    }
}

#[async_trait]
impl Tool for BoundedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.inner.input_schema()
    }

    fn class(&self) -> ToolClass {
        self.inner.class()
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = self.reporter.next_id();
        let name = self.name().to_owned();
        let class = self.class();
        self.reporter.send(ToolRunEvent::Started {
            id,
            name: name.clone(),
            input: input.clone(),
            class,
        });

        let started = Instant::now();
        match self.inner.execute(input, ctx).await {
            Ok(output) => {
                let duration = started.elapsed();
                let output_chars = output.content().chars().count();
                let is_error = output.is_error();
                let truncated = truncate_tool_output(output);
                let sent_chars = truncated.content().chars().count();
                let preview = tool_output_preview(truncated.content());

                self.reporter.send(ToolRunEvent::Finished {
                    id,
                    name,
                    class,
                    duration,
                    output_chars,
                    sent_chars,
                    is_error,
                    preview,
                });

                Ok(truncated)
            }
            Err(err) => {
                self.reporter.send(ToolRunEvent::Finished {
                    id,
                    name,
                    class,
                    duration: started.elapsed(),
                    output_chars: 0,
                    sent_chars: 0,
                    is_error: true,
                    preview: tool_output_preview(&err.to_string()),
                });

                Err(err)
            }
        }
    }
}

fn bounded_default_tools(reporter: ToolReporter) -> Vec<Arc<dyn Tool>> {
    tools::defaults()
        .into_iter()
        .map(|tool| Arc::new(BoundedTool::new(tool, reporter.clone())) as Arc<dyn Tool>)
        .collect()
}

fn truncate_tool_output(output: ToolOutput) -> ToolOutput {
    match output {
        ToolOutput::Text(text) => ToolOutput::text(truncate_text(text)),
        ToolOutput::Error(text) => ToolOutput::error(truncate_text(text)),
    }
}

fn truncate_text(text: String) -> String {
    let total_chars = text.chars().count();
    if total_chars <= MAX_TOOL_OUTPUT_CHARS {
        return text;
    }

    let mut truncated: String = text.chars().take(MAX_TOOL_OUTPUT_CHARS).collect();
    truncated.push_str(&format!(
        "\n\n[Sunny truncated tool output: showed {MAX_TOOL_OUTPUT_CHARS} of {total_chars} chars. Retry with a narrower command/read if needed.]"
    ));
    truncated
}

async fn handle_agent_message(
    agent: &Agent,
    line: &str,
    ui: &Ui,
    tool_reporter: &ToolReporter,
    messages: &mut Vec<Message>,
) -> Result<()> {
    messages.push(Message::user(vec![Content::text(line)]));

    match process_input(agent, messages, ui, tool_reporter).await {
        Ok(result) => {
            messages.extend(result.new_messages);
            Ok(())
        }
        Err(err) => {
            messages.pop();
            Err(err)
        }
    }
}

async fn process_input(
    agent: &Agent,
    messages: &[Message],
    ui: &Ui,
    tool_reporter: &ToolReporter,
) -> Result<AgentResult> {
    let token = CancellationToken::new();
    let mut tool_events = tool_reporter.subscribe();
    let mut stream = agent.stream(messages.to_vec(), token);
    let mut tool_stats = ToolRunStats::default();
    let run_started = Instant::now();
    let mut content = String::new();
    let mut assistant_started = false;
    let debug = stream_debug_enabled();

    let mut stream_open = true;
    while stream_open {
        tokio::select! {
            event = stream.next() => {
                let Some(event) = event else {
                    stream_open = false;
                    continue;
                };

                print_stream_event(ui, event?, &mut content, &mut assistant_started, debug)?;
            }
            event = tool_events.recv() => {
                match event {
                    Ok(event) => print_tool_event(ui, event, &mut tool_stats, &mut assistant_started)?,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        print_note(ui, format!("tool event stream skipped {skipped} events"));
                    }
                    Err(broadcast::error::RecvError::Closed) => {}
                }
            }
        }
    }

    drain_tool_events(
        ui,
        &mut tool_events,
        &mut tool_stats,
        &mut assistant_started,
    )?;

    let streamed = !content.is_empty();
    let result = stream.into_result().await?;

    if !streamed && !result.text.is_empty() {
        print!("{}{}", ui.assistant_prefix(), result.text);
        std::io::stdout().flush()?;
    }

    if streamed || !result.text.is_empty() {
        println!();
    }

    print_run_stats(ui, &tool_stats, run_started.elapsed(), &result)?;

    Ok(result)
}

fn print_stream_event(
    ui: &Ui,
    event: StreamEvent,
    content: &mut String,
    assistant_started: &mut bool,
    debug: bool,
) -> Result<()> {
    match event {
        StreamEvent::ContentDelta(delta) => {
            if !*assistant_started {
                print!("{}", ui.assistant_prefix());
                std::io::stdout().flush()?;
                *assistant_started = true;
            }

            print!("{delta}");
            std::io::stdout().flush()?;
            content.push_str(&delta);
        }
        StreamEvent::ToolUse { id, name, input } => {
            if debug {
                print_debug(ui, format!("tool_use {id} {name} {input}"));
            }
        }
        StreamEvent::ToolCallPending {
            id,
            name,
            input,
            class,
        } => {
            if debug {
                print_debug(ui, format!("tool_pending {id} {name} {input} {class:?}"));
            }
        }
        StreamEvent::MessageDelta { stop_reason } => {
            if debug {
                print_debug(ui, format!("stop_reason {stop_reason:?}"));
            }
        }
        StreamEvent::Usage(usage) => {
            if debug {
                print_debug(ui, format!("usage {usage:?}"));
            }
        }
        StreamEvent::Done => {
            if debug {
                print_debug(ui, "done");
            }
        }
    }

    Ok(())
}

async fn detect_command(
    line: &str,
    ui: &Ui,
    state: &mut State,
    agent: &mut Option<Agent>,
    creds_manager: &CredentialsManager,
    tool_reporter: &ToolReporter,
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
                *agent = Some(create_openai_agent(
                    credentials,
                    creds_manager.clone(),
                    tool_reporter.clone(),
                ));
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

#[derive(Default)]
struct ToolRunStats {
    calls: usize,
    completed: usize,
    failed: usize,
    read_only: usize,
    side_effect: usize,
    output_chars: usize,
    sent_chars: usize,
    tool_time: Duration,
    by_name: BTreeMap<String, usize>,
}

impl ToolRunStats {
    fn record_started(&mut self, name: &str, class: ToolClass) {
        self.calls += 1;
        match class {
            ToolClass::ReadOnly => self.read_only += 1,
            ToolClass::Mutating => self.side_effect += 1,
        }
        *self.by_name.entry(name.to_owned()).or_default() += 1;
    }

    fn record_finished(
        &mut self,
        duration: Duration,
        output_chars: usize,
        sent_chars: usize,
        is_error: bool,
    ) {
        self.completed += 1;
        if is_error {
            self.failed += 1;
        }
        self.tool_time += duration;
        self.output_chars += output_chars;
        self.sent_chars += sent_chars;
    }
}

fn print_tool_event(
    ui: &Ui,
    event: ToolRunEvent,
    stats: &mut ToolRunStats,
    assistant_started: &mut bool,
) -> Result<()> {
    if *assistant_started {
        println!();
        *assistant_started = false;
    }

    match event {
        ToolRunEvent::Started {
            id,
            name,
            input,
            class,
        } => {
            stats.record_started(&name, class);
            println!(
                "{} {} {} {} {}",
                ui.paint(FRAME, "↳"),
                ui.paint(ORANGE, tool_id(id)),
                ui.paint(tool_risk_color(&name, class), tool_risk_label(class)),
                ui.paint(CREAM, tool_action(&name)),
                ui.paint(CREAM, tool_detail(&name, &input))
            );
        }
        ToolRunEvent::Finished {
            id,
            name,
            class,
            duration,
            output_chars,
            sent_chars,
            is_error,
            preview,
        } => {
            stats.record_finished(duration, output_chars, sent_chars, is_error);
            let marker = if is_error { "✕" } else { "✓" };
            let marker_color = if is_error { RED } else { GREEN };
            let preview = if preview.is_empty() {
                String::new()
            } else {
                format!(" · {preview}")
            };

            println!(
                "  {} {} {} {} · {}{}",
                ui.paint(marker_color, marker),
                ui.paint(ORANGE, tool_id(id)),
                ui.paint(tool_risk_color(&name, class), tool_action(&name)),
                ui.paint(CREAM, format_duration(duration)),
                ui.paint(CREAM, format_output_size(output_chars, sent_chars)),
                ui.paint(DIM, preview)
            );
        }
    }

    std::io::stdout().flush()?;
    Ok(())
}

fn drain_tool_events(
    ui: &Ui,
    tool_events: &mut broadcast::Receiver<ToolRunEvent>,
    stats: &mut ToolRunStats,
    assistant_started: &mut bool,
) -> Result<()> {
    loop {
        match tool_events.try_recv() {
            Ok(event) => print_tool_event(ui, event, stats, assistant_started)?,
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                print_note(ui, format!("tool event stream skipped {skipped} events"));
            }
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                break;
            }
        }
    }

    Ok(())
}

fn print_run_stats(
    ui: &Ui,
    stats: &ToolRunStats,
    wall_time: Duration,
    result: &AgentResult,
) -> Result<()> {
    if stats.calls == 0 {
        return Ok(());
    }

    let ok = stats.completed.saturating_sub(stats.failed);
    println!(
        "{} tools {} calls · {} ok/{} error · wall {} · tool {} · output {} · {}",
        ui.paint(DIM, "stats:"),
        ui.paint(CREAM, stats.calls.to_string()),
        ui.paint(GREEN, ok.to_string()),
        ui.paint(
            if stats.failed == 0 { GREEN } else { RED },
            stats.failed.to_string()
        ),
        ui.paint(CREAM, format_duration(wall_time)),
        ui.paint(CREAM, format_duration(stats.tool_time)),
        ui.paint(
            CREAM,
            format_output_size(stats.output_chars, stats.sent_chars)
        ),
        ui.paint(CREAM, tool_breakdown(stats))
    );

    let usage = &result.usage;
    let total_tokens = usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .saturating_add(usage.cache_creation_input_tokens)
        .saturating_add(usage.cache_read_input_tokens);
    if total_tokens > 0 {
        println!(
            "{} model in {} · out {}",
            ui.paint(DIM, "stats:"),
            ui.paint(
                CREAM,
                format_compact_count(usage.input_tokens as usize, "tok")
            ),
            ui.paint(
                CREAM,
                format_compact_count(usage.output_tokens as usize, "tok")
            )
        );
    }

    std::io::stdout().flush()?;
    Ok(())
}

fn tool_id(id: u64) -> String {
    format!("#{id:02}")
}

fn tool_risk_label(class: ToolClass) -> &'static str {
    match class {
        ToolClass::ReadOnly => "read-only",
        ToolClass::Mutating => "side-effect",
    }
}

fn tool_risk_color(name: &str, class: ToolClass) -> &'static str {
    match (name, class) {
        ("write" | "edit", _) => RED,
        ("bash", _) | (_, ToolClass::Mutating) => ORANGE,
        (_, ToolClass::ReadOnly) => GREEN,
    }
}

fn tool_action(name: &str) -> String {
    match name {
        "bash" => "SHELL".to_owned(),
        "edit" => "EDIT FILE".to_owned(),
        "glob" => "FIND FILES".to_owned(),
        "grep" => "SEARCH".to_owned(),
        "read" => "READ FILE".to_owned(),
        "write" => "WRITE FILE".to_owned(),
        _ => name.replace('_', " ").to_uppercase(),
    }
}

fn tool_detail(name: &str, input: &Value) -> String {
    let detail = match name {
        "bash" => bash_detail(input),
        "edit" => edit_detail(input),
        "glob" => glob_detail(input),
        "grep" => grep_detail(input),
        "read" => read_detail(input),
        "write" => write_detail(input),
        _ => fallback_detail(input),
    };

    if detail.is_empty() {
        compact_json(input, 220)
    } else {
        detail
    }
}

fn bash_detail(input: &Value) -> String {
    join_parts([
        string_field(input, "command", "cmd", 140),
        millis_field(input, "timeout_ms", "timeout"),
    ])
}

fn edit_detail(input: &Value) -> String {
    join_parts([
        path_field(input),
        string_len_field(input, "old_string", "old"),
        string_len_field(input, "new_string", "new"),
        scalar_field(input, "replace_all", "all"),
    ])
}

fn glob_detail(input: &Value) -> String {
    join_parts([
        string_field(input, "path", "path", 80),
        string_field(input, "pattern", "pattern", 120),
    ])
}

fn grep_detail(input: &Value) -> String {
    join_parts([
        string_field(input, "pattern", "pattern", 120),
        string_field(input, "path", "path", 80),
        string_field(input, "glob", "glob", 80),
        scalar_field(input, "max_results", "max"),
    ])
}

fn read_detail(input: &Value) -> String {
    join_parts([
        path_field(input),
        scalar_field(input, "offset", "from"),
        scalar_field(input, "limit", "limit"),
    ])
}

fn write_detail(input: &Value) -> String {
    join_parts([
        path_field(input),
        string_len_field(input, "content", "content"),
    ])
}

fn fallback_detail(input: &Value) -> String {
    let Some(object) = input.as_object() else {
        return compact_json(input, 220);
    };

    let parts: Vec<String> = object
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "content" | "oldText" | "newText" | "old_string" | "new_string" | "value"
            )
        })
        .take(5)
        .map(|(key, value)| format!("{key}={}", compact_value(value, 80)))
        .collect();

    parts.join(" ")
}

fn path_field(input: &Value) -> Option<String> {
    string_field(input, "file_path", "file", 120)
        .or_else(|| string_field(input, "path", "path", 120))
}

fn string_field(input: &Value, key: &str, label: &str, max_chars: usize) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(|value| format!("{label}={}", compact_string(value, max_chars)))
}

fn string_len_field(input: &Value, key: &str, label: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(|value| format!("{label}={} chars", value.chars().count()))
}

fn scalar_field(input: &Value, key: &str, label: &str) -> Option<String> {
    let value = input.get(key)?;
    if value.is_string() || value.is_number() || value.is_boolean() {
        Some(format!("{label}={}", compact_value(value, 80)))
    } else {
        None
    }
}

fn millis_field(input: &Value, key: &str, label: &str) -> Option<String> {
    let millis = input.get(key)?.as_u64()?;
    let value = if millis >= 1_000 && millis % 1_000 == 0 {
        format!("{}s", millis / 1_000)
    } else {
        format!("{millis}ms")
    };

    Some(format!("{label}={value}"))
}

fn join_parts(parts: impl IntoIterator<Item = Option<String>>) -> String {
    parts.into_iter().flatten().collect::<Vec<_>>().join(" ")
}

fn compact_value(value: &Value, max_chars: usize) -> String {
    match value {
        Value::String(value) => compact_string(value, max_chars),
        Value::Array(items) => format!("[{} items]", items.len()),
        Value::Object(fields) => format!("{{{} fields}}", fields.len()),
        _ => value.to_string(),
    }
}

fn compact_string(value: &str, max_chars: usize) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_owned();
    }

    let mut compact: String = value.chars().take(max_chars).collect();
    compact.push('…');
    compact
}

fn compact_json(value: &Value, max_chars: usize) -> String {
    compact_string(&value.to_string(), max_chars)
}

fn tool_output_preview(output: &str) -> String {
    let normalized = output
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if normalized == "(no output)" {
        String::new()
    } else {
        compact_string(&normalized, 180)
    }
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        return format!("{millis}ms");
    }

    let seconds = duration.as_secs_f64();
    if seconds < 10.0 {
        format!("{seconds:.1}s")
    } else {
        format!("{seconds:.0}s")
    }
}

fn format_output_size(output_chars: usize, sent_chars: usize) -> String {
    let output = format_compact_count(output_chars, "chars");
    if output_chars == sent_chars {
        format!("{output} returned")
    } else {
        format!(
            "{output} returned · {} sent",
            format_compact_count(sent_chars, "chars")
        )
    }
}

fn format_compact_count(count: usize, unit: &str) -> String {
    if count >= 1_000_000 {
        format!("{:.1}m {unit}", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k {unit}", count as f64 / 1_000.0)
    } else {
        format!("{count} {unit}")
    }
}

fn tool_breakdown(stats: &ToolRunStats) -> String {
    let mut parts: Vec<String> = stats
        .by_name
        .iter()
        .map(|(name, count)| format!("{name}×{count}"))
        .collect();

    if parts.len() > 6 {
        let hidden = parts.len() - 6;
        parts.truncate(6);
        parts.push(format!("+{hidden} more"));
    }

    parts.join(" ")
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
