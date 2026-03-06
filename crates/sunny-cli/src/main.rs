use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "sunny")]
#[command(about = "Sunny - AI Agent Runtime")]
enum Cli {
    /// Analyze a codebase
    Analyze(crate::commands::AnalyzeArgs),
    /// Send a prompt to the agent
    Prompt(crate::commands::PromptArgs),
}

mod commands;
mod output;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli {
        Cli::Analyze(args) => {
            if let Err(e) = commands::analyze::run_analyze(args).await {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        Cli::Prompt(args) => {
            if let Err(e) = commands::prompt::run_prompt(args).await {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse_analyze_subcommand() {
        let cli = Cli::try_parse_from(["sunny", "analyze", "."]);
        assert!(cli.is_ok(), "analyze subcommand should parse");
    }

    #[test]
    fn test_cli_parse_prompt_subcommand() {
        let cli = Cli::try_parse_from(["sunny", "prompt", "hello"]);
        assert!(cli.is_ok(), "prompt subcommand should parse");
    }

    #[test]
    fn test_cli_parse_prompt_with_flags() {
        let cli =
            Cli::try_parse_from(["sunny", "prompt", "hello", "--format", "json", "--dry-run"]);
        assert!(cli.is_ok(), "prompt with flags should parse");
    }
}
