use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use sunny_boys::agent::approval::AlwaysAllowGate;
use sunny_boys::{AgentSession, SharedApprovalGate};
use sunny_core::tool::InterviewAnswer;
use sunny_mind::{AnthropicProvider, LlmProvider};
use sunny_store::{Database, SessionStore};

use crate::bridge::{AgentEventSender, AgentMode, AgentToTui, TuiCommandReceiver, TuiToAgent};
use crate::interview_presenter::TuiInterviewPresenter;

pub fn spawn_agent_bridge(
    workspace_root: PathBuf,
    session_id: Option<String>,
) -> anyhow::Result<(mpsc::Receiver<AgentToTui>, mpsc::Sender<TuiToAgent>)> {
    let (event_tx, event_rx) = mpsc::channel::<AgentToTui>(64);
    let (cmd_tx, cmd_rx) = mpsc::channel::<TuiToAgent>(32);

    tokio::task::spawn_local(bridge_loop(workspace_root, session_id, event_tx, cmd_rx));

    Ok((event_rx, cmd_tx))
}

async fn bridge_loop(
    workspace_root: PathBuf,
    session_id: Option<String>,
    tx: AgentEventSender,
    mut rx: TuiCommandReceiver,
) {
    let provider = match AnthropicProvider::new("claude-sonnet-4-6") {
        Ok(provider) => Arc::new(provider) as Arc<dyn LlmProvider>,
        Err(err) => {
            error!(error = %err, "failed to create AnthropicProvider");
            let _ = tx
                .send(AgentToTui::Error(format!("Provider init failed: {err}")))
                .await;
            return;
        }
    };

    let store = match Database::open_default()
        .context("failed to open database")
        .map(SessionStore::new)
    {
        Ok(store) => {
            #[allow(clippy::arc_with_non_send_sync)]
            let store = Arc::new(store);
            store
        }
        Err(err) => {
            error!(error = %err, "failed to open session DB");
            let _ = tx
                .send(AgentToTui::Error(format!("DB open failed: {err}")))
                .await;
            return;
        }
    };

    let approval_gate = Arc::new(AlwaysAllowGate) as SharedApprovalGate;

    // Create TUI interview presenter.  The forwarder task is spawned on the
    // thread-pool (not spawn_local) so it can run while the LocalSet thread
    // is blocked inside block_in_place during a tool call.
    let (presenter, mut interview_rx) = TuiInterviewPresenter::new();
    let tx_for_interview = tx.clone();
    tokio::spawn(async move {
        while let Some(req) = interview_rx.recv().await {
            let (resp_tx, mut resp_rx) = mpsc::channel::<Vec<InterviewAnswer>>(1);
            let id = uuid::Uuid::new_v4().to_string();

            if tx_for_interview
                .send(AgentToTui::InterviewRequest {
                    id: id.clone(),
                    questions: req.questions,
                    response_tx: resp_tx,
                })
                .await
                .is_err()
            {
                // TUI gone — send an error so the tool call can unblock.
                let _ = req.response_tx.send(Err(sunny_core::tool::ToolError::ExecutionFailed {
                    source: Box::new(std::io::Error::other("TUI channel closed")),
                }));
                continue;
            }

            match resp_rx.recv().await {
                Some(answers) => {
                    let _ = req.response_tx.send(Ok(answers));
                }
                None => {
                    let _ = req.response_tx.send(Err(sunny_core::tool::ToolError::ExecutionFailed {
                        source: Box::new(std::io::Error::other("Interview cancelled")),
                    }));
                }
            }
        }
    });

    let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let loaded = tokio::task::spawn_blocking({
        let session_id = session_id.clone();
        move || -> Result<_, sunny_store::StoreError> {
            let db = Database::open_default()?;
            let store = SessionStore::new(db);
            let saved = store.load_session(&session_id)?;
            let messages = if saved.is_some() {
                store.load_messages(&session_id)?
            } else {
                Vec::new()
            };
            Ok((saved, messages))
        }
    })
    .await;

    let presenter: Arc<dyn sunny_boys::InterviewPresenter> = Arc::new(presenter);

    let mut session = match loaded {
        Ok(Ok((Some(saved), messages))) => AgentSession::from_saved(
            Arc::clone(&store),
            saved,
            messages,
            Arc::clone(&provider),
            workspace_root.clone(),
            CancellationToken::new(),
        )
        .with_approval_gate(Arc::clone(&approval_gate))
        .with_interview_presenter(Arc::clone(&presenter)),
        Ok(Ok((None, _))) => AgentSession::new(
            Arc::clone(&provider),
            workspace_root.clone(),
            session_id.clone(),
            Arc::clone(&store),
        )
        .with_approval_gate(Arc::clone(&approval_gate))
        .with_interview_presenter(Arc::clone(&presenter)),
        Ok(Err(err)) => {
            warn!(session_id = %session_id, error = %err, "failed to load saved session");
            AgentSession::new(
                Arc::clone(&provider),
                workspace_root.clone(),
                session_id.clone(),
                Arc::clone(&store),
            )
            .with_approval_gate(Arc::clone(&approval_gate))
            .with_interview_presenter(Arc::clone(&presenter))
        }
        Err(err) => {
            warn!(session_id = %session_id, error = %err, "failed to join saved session load task");
            AgentSession::new(
                Arc::clone(&provider),
                workspace_root.clone(),
                session_id.clone(),
                Arc::clone(&store),
            )
            .with_approval_gate(Arc::clone(&approval_gate))
            .with_interview_presenter(Arc::clone(&presenter))
        }
    };

    if tx
        .send(AgentToTui::SessionReady {
            session_id: session_id.clone(),
        })
        .await
        .is_err()
    {
        return;
    }

    info!(session_id = %session_id, "agent bridge loop started");

    let mut is_streaming = false;

    while let Some(cmd) = rx.recv().await {
        match cmd {
            TuiToAgent::SendMessage(text) => {
                is_streaming = true;
                if tx.send(AgentToTui::StreamingStarted).await.is_err() {
                    break;
                }

                let tx_clone = tx.clone();
                let result = session
                    .send(&text, move |event| {
                        let _ = tx_clone.try_send(AgentToTui::StreamChunk(event));
                    })
                    .await;

                is_streaming = false;
                match result {
                    Ok(_) => {
                        if tx.send(AgentToTui::StreamingDone).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        warn!(error = %err, "agent send error");
                        if tx
                            .send(AgentToTui::Error(format!("Agent error: {err}")))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            TuiToAgent::CancelStream => {
                session.cancel_current();
            }
            TuiToAgent::ApprovalResponse { .. } => {
                // TODO(v2): Wire to SharedApprovalGate when TUI-aware gate is implemented
                warn!("ApprovalResponse received — TODO(v2): wire to TUI-aware approval gate");
            }
            TuiToAgent::SetMode(mode) => {
                if is_streaming {
                    warn!("SetMode received while streaming — ignored");
                    continue;
                }
                let plan_mode = match mode {
                    AgentMode::Quick => sunny_plans::model::PlanMode::Quick,
                    AgentMode::Smart => sunny_plans::model::PlanMode::Smart,
                };
                let plan_id = match plan_mode {
                    sunny_plans::model::PlanMode::Smart => uuid::Uuid::new_v4().to_string(),
                    sunny_plans::model::PlanMode::Quick => String::new(),
                };
                session.set_mode(plan_mode, &plan_id);
                let _ = tx.send(AgentToTui::ModeChanged(mode)).await;
            }
        }
    }

    info!("agent bridge loop ended");
}
