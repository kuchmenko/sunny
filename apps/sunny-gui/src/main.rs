//! sunny-gui — Sunny desktop GUI (egui/eframe).

use clap::Parser;

mod app;
mod approval;
mod bridge;
mod sessions;
mod theme;
mod widgets;

/// CLI arguments for sunny-gui.
#[derive(Parser, Debug)]
#[command(name = "sunny-gui")]
#[command(about = "Sunny — AI coding assistant GUI")]
pub struct Args {
    /// Path to workspace root (auto-detected if omitted).
    #[arg(long)]
    pub workspace: Option<std::path::PathBuf>,
}

fn detect_workspace_root(override_path: Option<std::path::PathBuf>) -> std::path::PathBuf {
    if let Some(path) = override_path {
        return path;
    }
    // Try git rev-parse --show-toplevel
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if output.status.success() {
            if let Ok(path_str) = std::str::from_utf8(&output.stdout) {
                let trimmed = path_str.trim();
                if !trimmed.is_empty() {
                    return std::path::PathBuf::from(trimmed);
                }
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .init();

    let args = Args::parse();
    let workspace_root = detect_workspace_root(args.workspace);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sunny")
            .with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };

    eframe::run_native(
        "sunny-gui",
        native_options,
        Box::new(move |cc| {
            // Apply brand theme
            theme::load_fonts(&cc.egui_ctx);
            cc.egui_ctx.set_visuals(theme::sunny_visuals());

            // Spawn the agent bridge thread
            let (tx_cmd, rx_evt, approval_rx, pending_approvals) =
                bridge::spawn_agent_bridge(workspace_root.clone(), cc.egui_ctx.clone());

            Ok(Box::new(app::SunnyApp::new(
                rx_evt,
                tx_cmd,
                approval_rx,
                pending_approvals,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    Ok(())
}
