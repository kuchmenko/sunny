use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::ChildStdout;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use crate::tool::lsp::jsonrpc::decode_content_length;

pub fn spawn_reader_task(
    stdout: ChildStdout,
    pending: Arc<DashMap<u32, oneshot::Sender<Value>>>,
    notification_tx: broadcast::Sender<Value>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);

        loop {
            let mut content_length = None;

            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => return,
                    Ok(_) => {
                        let trimmed = line.trim_end_matches(['\r', '\n']);
                        if trimmed.is_empty() {
                            break;
                        }

                        if content_length.is_none() {
                            content_length = decode_content_length(trimmed);
                        }
                    }
                    Err(_) => return,
                }
            }

            let Some(body_len) = content_length else {
                continue;
            };

            let mut body = vec![0_u8; body_len];
            if reader.read_exact(&mut body).await.is_err() {
                return;
            }

            let message: Value = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(_) => continue,
            };

            let has_id = message.get("id").is_some();
            let has_result_or_error =
                message.get("result").is_some() || message.get("error").is_some();

            if has_id && has_result_or_error {
                let Some(id) = message
                    .get("id")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                else {
                    continue;
                };

                if let Some((_, sender)) = pending.remove(&id) {
                    let _ = sender.send(message);
                }
                continue;
            }

            let is_notification = message.get("method").is_some() && message.get("id").is_none();
            if is_notification {
                let _ = notification_tx.send(message);
            }
        }
    })
}
