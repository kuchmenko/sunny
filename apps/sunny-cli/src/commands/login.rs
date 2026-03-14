use anyhow::Context;

pub async fn run() -> anyhow::Result<()> {
    let ctx = sunny_mind::build_login_context().context("Failed to build OAuth login context")?;

    eprintln!("Opening browser for authentication...");

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
        .context("OAuth login failed")?;

    eprintln!("Authenticated. Credentials saved to ~/.sunny/credentials.json");
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
