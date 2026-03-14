use clap::{Parser, Subcommand};
use sunny_cli::commands::ChatArgs;

#[derive(Parser, Debug)]
#[command(name = "sunny")]
#[command(about = "Sunny — AI coding assistant")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    chat: ChatArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Authenticate with Anthropic (Claude Max subscription required).
    Login,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Login) => sunny_cli::commands::login::run().await,
        None => sunny_cli::commands::chat::run(cli.chat).await,
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse_default_no_args() {
        let cli =
            Cli::try_parse_from(["sunny"]).expect("bare sunny should parse without subcommand");
        assert!(cli.command.is_none(), "no subcommand means chat mode");
    }

    #[test]
    fn test_cli_parse_with_model_flag() {
        let cli = Cli::try_parse_from(["sunny", "--model", "claude-3-5-sonnet"])
            .expect("sunny --model should parse");
        assert!(cli.command.is_none(), "--model should not set a subcommand");
    }

    #[test]
    fn test_cli_parse_with_api_key_flag() {
        let cli = Cli::try_parse_from(["sunny", "--api-key", "test-key"])
            .expect("sunny --api-key should parse");
        assert!(
            cli.command.is_none(),
            "--api-key should not set a subcommand"
        );
    }

    #[test]
    fn test_cli_parse_login_subcommand() {
        let cli = Cli::try_parse_from(["sunny", "login"])
            .expect("sunny login should parse as subcommand");
        assert!(
            matches!(cli.command, Some(Command::Login)),
            "login subcommand must be parsed as Command::Login"
        );
    }

    #[test]
    fn test_cli_parse_continue_flag() {
        let cli = Cli::try_parse_from(["sunny", "--continue"])
            .expect("sunny --continue should still work");
        assert!(
            cli.command.is_none(),
            "--continue should not set a subcommand"
        );
    }
}
