use clap::Args;
use sunny_tasks::WorkspaceDetector;
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Args, Debug)]
pub struct WorkArgs {
    /// Maximum number of tasks to run concurrently
    #[arg(long, default_value_t = 3)]
    pub max_concurrent: usize,
}

pub async fn run(args: WorkArgs) -> anyhow::Result<()> {
    let git_root =
        WorkspaceDetector::detect_cwd().ok_or_else(|| anyhow::anyhow!("no git workspace found"))?;

    info!(max_concurrent = args.max_concurrent, git_root = ?git_root, "worker starting");

    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received shutdown signal");
            cancel.cancel();
        }
        _ = worker_cancel.cancelled() => {}
    }

    info!("worker stopped");
    Ok(())
}
