use clap::{Parser, Subcommand};
use sunny_cli::commands::{ChatArgs, TasksArgs};

#[derive(Parser, Debug)]
#[command(name = "sunny")]
#[command(about = "Sunny — AI coding assistant")]
struct Cli {
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    chat: ChatArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Authenticate with Anthropic (Claude Max subscription required).
    Login,
    /// Manage autonomous task records in the current workspace.
    Tasks(TasksArgs),
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    let filter = if cli.verbose {
        tracing_subscriber::EnvFilter::new("debug")
    } else {
        tracing_subscriber::EnvFilter::new("warn")
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let result = match cli.command {
        Some(Command::Login) => sunny_cli::commands::login::run().await,
        Some(Command::Tasks(args)) => sunny_cli::commands::tasks::run(args).await,
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
    fn test_cli_parse_tasks_subcommand() {
        let cli = Cli::try_parse_from(["sunny", "tasks", "list"])
            .expect("sunny tasks list should parse as subcommand");
        assert!(
            matches!(cli.command, Some(Command::Tasks(_))),
            "tasks subcommand must be parsed as Command::Tasks"
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

    #[test]
    fn test_verbose_flag_default() {
        let cli = Cli::try_parse_from(["sunny"]).expect("bare sunny should parse");
        assert!(!cli.verbose, "verbose should default to false");
    }

    #[test]
    fn test_verbose_flag_enabled() {
        let cli =
            Cli::try_parse_from(["sunny", "--verbose"]).expect("sunny --verbose should parse");
        assert!(cli.verbose, "--verbose flag should set verbose to true");
    }
}
