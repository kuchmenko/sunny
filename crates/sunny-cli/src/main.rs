use clap::Parser;
use sunny_cli::commands::ChatArgs;

#[derive(Parser, Debug)]
#[command(name = "sunny")]
#[command(about = "Sunny — AI coding assistant")]
struct Cli {
    #[command(flatten)]
    chat: ChatArgs,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    if let Err(e) = sunny_cli::commands::chat::run(cli.chat).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse_default_no_args() {
        let cli = Cli::try_parse_from(["sunny"]);
        assert!(cli.is_ok(), "bare sunny should parse without subcommand");
    }

    #[test]
    fn test_cli_parse_with_model_flag() {
        let cli = Cli::try_parse_from(["sunny", "--model", "claude-3-5-sonnet"]);
        assert!(cli.is_ok(), "sunny --model should parse");
    }

    #[test]
    fn test_cli_parse_with_api_key_flag() {
        let cli = Cli::try_parse_from(["sunny", "--api-key", "test-key"]);
        assert!(cli.is_ok(), "sunny --api-key should parse");
    }
}
