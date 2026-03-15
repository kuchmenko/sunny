use anyhow::Context;
use clap::Args;

/// Arguments for the `sunny login` subcommand.
#[derive(Args, Debug)]
pub struct LoginArgs {
    /// Provider to authenticate with. Defaults to "anthropic".
    #[arg(long, default_value = "anthropic")]
    pub provider: String,

    /// Shorthand for `--provider openai`.
    #[arg(long)]
    pub openai: bool,
}

pub async fn run(args: LoginArgs) -> anyhow::Result<()> {
    // --openai is shorthand for --provider openai.
    let provider = if args.openai {
        "openai"
    } else {
        args.provider.as_str()
    };

    match provider {
        "openai" => run_openai_login().await,
        _ => run_anthropic_login().await,
    }
}

async fn run_anthropic_login() -> anyhow::Result<()> {
    let ctx = sunny_mind::build_login_context().context("Failed to build OAuth login context")?;

    eprintln!("Opening browser for Anthropic authentication...");

    let opened = open_browser(&ctx.authorize_url);
    if !opened {
        eprintln!("Open this URL in your browser:");
        eprintln!("{}", ctx.authorize_url);
    }

    eprintln!();
    eprintln!("After authenticating, paste the code from the browser (format: code#state):");

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read authorization code from stdin")?;

    let client = reqwest::Client::new();
    sunny_mind::complete_login(&client, &ctx, &input)
        .await
        .context("Anthropic OAuth login failed")?;

    eprintln!("Authenticated. Credentials saved to ~/.sunny/credentials.json");
    Ok(())
}

async fn run_openai_login() -> anyhow::Result<()> {
    use sunny_mind::openai::oauth::{build_login_context, complete_oauth_from_context};

    let ctx = build_login_context().context("Failed to build OpenAI OAuth login context")?;

    eprintln!("Opening browser for OpenAI authentication...");
    eprintln!("A local callback server will be started on port 1455.");
    eprintln!();

    let opened = open_browser(&ctx.authorize_url);
    if !opened {
        eprintln!("Open this URL in your browser:");
        eprintln!("{}", ctx.authorize_url);
    }

    eprintln!();
    eprintln!("Waiting for browser redirect callback on http://localhost:1455/auth/callback ...");

    let client = reqwest::Client::new();
    complete_oauth_from_context(&client, &ctx)
        .await
        .context("OpenAI OAuth login failed")?;

    eprintln!("OpenAI login successful! Credentials saved to ~/.sunny/openai_credentials.json");
    eprintln!();
    eprintln!("Available models:");
    eprintln!("  gpt-5.4, gpt-5.3-codex, gpt-5.3-codex-spark");
    Ok(())
}

fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn().is_ok()
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn()
            .is_ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .is_ok()
    }
}
