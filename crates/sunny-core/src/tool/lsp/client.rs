use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use lsp_types::notification::{
    DidOpenTextDocument, Exit, Initialized, Notification as LspNotification,
};
use lsp_types::request::{Initialize, Request as LspRequest, Shutdown};
use lsp_types::{
    ClientCapabilities, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    InitializedParams, TextDocumentItem,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::warn;
use url::Url;

use crate::tool::lsp::jsonrpc::{encode, Notification, Request, Response};
use crate::tool::lsp::transport::spawn_reader_task;
use crate::tool::ToolError;

struct LspProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    reader_task: JoinHandle<()>,
}

pub struct LspClient {
    process: Mutex<LspProcess>,
    command: String,
    root: PathBuf,
    request_counter: Arc<AtomicU32>,
    pending_requests: Arc<DashMap<u32, oneshot::Sender<Value>>>,
    notification_tx: broadcast::Sender<Value>,
    initialized: Arc<AtomicBool>,
}

impl LspClient {
    pub async fn spawn(command: &str, root: &Path) -> Result<Self, ToolError> {
        validate_command(command).await?;

        let pending_requests = Arc::new(DashMap::new());
        let (notification_tx, _) = broadcast::channel(128);
        let process = spawn_process(
            command,
            root,
            pending_requests.clone(),
            notification_tx.clone(),
        )
        .await?;

        Ok(Self {
            process: Mutex::new(process),
            command: command.to_string(),
            root: root.to_path_buf(),
            request_counter: Arc::new(AtomicU32::new(1)),
            pending_requests,
            notification_tx,
            initialized: Arc::new(AtomicBool::new(false)),
        })
    }

    pub async fn initialize(&mut self, root: &Path) -> Result<(), ToolError> {
        self.root = root.to_path_buf();
        self.ensure_process().await?;
        match self.initialize_internal(root).await {
            Ok(()) => Ok(()),
            Err(_) => {
                self.restart().await?;
                self.initialize_internal(root).await
            }
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), ToolError> {
        self.request::<Shutdown>(()).await?;
        self.notify::<Exit>(()).await?;
        self.initialized.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        self.initialized.load(Ordering::Relaxed)
    }

    pub async fn did_open(&self, path: &Path, text: &str) -> Result<(), ToolError> {
        let uri = Url::from_file_path(path)
            .map_err(|_| {
                execution_error(&format!(
                    "failed to convert path to file URL: {}",
                    path.display()
                ))
            })?
            .to_string()
            .parse()
            .map_err(|source| ToolError::ExecutionFailed {
                source: Box::new(source),
            })?;
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri,
                language_id: "rust".to_string(),
                version: 0,
                text: text.to_string(),
            },
        };

        self.notify::<DidOpenTextDocument>(params).await
    }

    pub async fn restart(&self) -> Result<(), ToolError> {
        let was_ready = self.is_ready();
        self.initialized.store(false, Ordering::Relaxed);
        self.pending_requests.clear();
        let delays = [1_u64, 2, 4];
        let mut last_error = None;

        for delay_secs in delays {
            warn!(
                operation = "lsp_restart",
                command = %self.command,
                root = %self.root.display(),
                delay_secs,
                "LSP subprocess exited; attempting restart"
            );
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;

            match spawn_process(
                &self.command,
                &self.root,
                self.pending_requests.clone(),
                self.notification_tx.clone(),
            )
            .await
            {
                Ok(new_process) => {
                    let mut process = self.process.lock().await;
                    let mut old_process = std::mem::replace(&mut *process, new_process);
                    old_process.reader_task.abort();
                    let _ = old_process.child.start_kill();
                    drop(process);

                    if was_ready {
                        self.initialize_internal(&self.root).await?;
                    }

                    return Ok(());
                }
                Err(err) => last_error = Some(err),
            }
        }

        Err(last_error.unwrap_or_else(|| execution_error("failed to restart LSP subprocess")))
    }

    pub async fn request<R: LspRequest>(&self, params: R::Params) -> Result<R::Result, ToolError>
    where
        R::Params: Serialize,
        R::Result: DeserializeOwned,
    {
        let id = self.request_counter.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending_requests.insert(id, tx);

        let params_value =
            serde_json::to_value(params).map_err(|source| ToolError::ExecutionFailed {
                source: Box::new(source),
            })?;
        let request = Request {
            jsonrpc: "2.0".to_string(),
            id,
            method: R::METHOD.to_string(),
            params: Some(params_value),
        };

        if let Err(err) = self.send_message(&request).await {
            self.pending_requests.remove(&id);
            return Err(err);
        }

        receive_response::<R>(rx, None).await
    }

    async fn request_direct<R: LspRequest>(&self, params: R::Params) -> Result<R::Result, ToolError>
    where
        R::Params: Serialize,
        R::Result: DeserializeOwned,
    {
        let id = self.request_counter.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending_requests.insert(id, tx);

        let params_value =
            serde_json::to_value(params).map_err(|source| ToolError::ExecutionFailed {
                source: Box::new(source),
            })?;
        let request = Request {
            jsonrpc: "2.0".to_string(),
            id,
            method: R::METHOD.to_string(),
            params: Some(params_value),
        };

        if let Err(err) = self.send_message_direct(&request).await {
            self.pending_requests.remove(&id);
            return Err(err);
        }

        receive_response::<R>(rx, Some(Duration::from_secs(2))).await
    }

    pub async fn notify<N: LspNotification>(&self, params: N::Params) -> Result<(), ToolError>
    where
        N::Params: Serialize,
    {
        let params_value =
            serde_json::to_value(params).map_err(|source| ToolError::ExecutionFailed {
                source: Box::new(source),
            })?;
        let notification = Notification {
            jsonrpc: "2.0".to_string(),
            method: N::METHOD.to_string(),
            params: Some(params_value),
        };

        self.send_message(&notification).await
    }

    async fn notify_direct<N: LspNotification>(&self, params: N::Params) -> Result<(), ToolError>
    where
        N::Params: Serialize,
    {
        let params_value =
            serde_json::to_value(params).map_err(|source| ToolError::ExecutionFailed {
                source: Box::new(source),
            })?;
        let notification = Notification {
            jsonrpc: "2.0".to_string(),
            method: N::METHOD.to_string(),
            params: Some(params_value),
        };

        self.send_message_direct(&notification).await
    }

    pub fn notification_rx(&self) -> broadcast::Receiver<Value> {
        self.notification_tx.subscribe()
    }

    async fn initialize_internal(&self, root: &Path) -> Result<(), ToolError> {
        let root_uri = Url::from_file_path(root)
            .map_err(|_| {
                execution_error(&format!(
                    "failed to convert path to file URL: {}",
                    root.display()
                ))
            })?
            .to_string()
            .parse()
            .map_err(|source| ToolError::ExecutionFailed {
                source: Box::new(source),
            })?;
        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: None,
            root_uri: Some(root_uri),
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        };

        let _: InitializeResult = self.request_direct::<Initialize>(params).await?;
        self.notify_direct::<Initialized>(InitializedParams {})
            .await?;
        self.initialized.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn send_message(&self, message: &impl Serialize) -> Result<(), ToolError> {
        let encoded = encode(message).map_err(|source| ToolError::ExecutionFailed {
            source: Box::new(source),
        })?;

        let mut has_retried = false;
        loop {
            self.ensure_process().await?;

            let mut process = self.process.lock().await;
            let write_result = async {
                process.stdin.write_all(&encoded).await?;
                process.stdin.flush().await
            }
            .await;

            match write_result {
                Ok(()) => return Ok(()),
                Err(_source) if !has_retried => {
                    self.initialized.store(false, Ordering::Relaxed);
                    drop(process);
                    self.restart().await?;
                    has_retried = true;
                }
                Err(source) => {
                    return Err(ToolError::ExecutionFailed {
                        source: Box::new(source),
                    });
                }
            }
        }
    }

    async fn send_message_direct(&self, message: &impl Serialize) -> Result<(), ToolError> {
        let encoded = encode(message).map_err(|source| ToolError::ExecutionFailed {
            source: Box::new(source),
        })?;

        let mut process = self.process.lock().await;
        process
            .stdin
            .write_all(&encoded)
            .await
            .map_err(|source| ToolError::ExecutionFailed {
                source: Box::new(source),
            })?;
        process
            .stdin
            .flush()
            .await
            .map_err(|source| ToolError::ExecutionFailed {
                source: Box::new(source),
            })?;

        Ok(())
    }

    async fn ensure_process(&self) -> Result<(), ToolError> {
        let exited = {
            let mut process = self.process.lock().await;
            process
                .child
                .try_wait()
                .map_err(|source| ToolError::ExecutionFailed {
                    source: Box::new(source),
                })?
                .is_some()
        };

        if exited {
            self.restart().await?;
        }

        Ok(())
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let process = self.process.get_mut();
        process.reader_task.abort();
        let _ = process.child.start_kill();
    }
}

async fn validate_command(command: &str) -> Result<(), ToolError> {
    if command == "rust-analyzer" {
        let check_status = Command::new("which")
            .arg("rust-analyzer")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map_err(|source| ToolError::ExecutionFailed {
                source: Box::new(source),
            })?;

        if !check_status.success() {
            return Err(execution_error("rust-analyzer not found in PATH"));
        }
    }

    Ok(())
}

async fn spawn_process(
    command: &str,
    root: &Path,
    pending_requests: Arc<DashMap<u32, oneshot::Sender<Value>>>,
    notification_tx: broadcast::Sender<Value>,
) -> Result<LspProcess, ToolError> {
    let mut child = Command::new(command)
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| ToolError::ExecutionFailed {
            source: Box::new(source),
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| execution_error("failed to capture LSP subprocess stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| execution_error("failed to capture LSP subprocess stdout"))?;

    let reader_task = spawn_reader_task(stdout, pending_requests, notification_tx);

    Ok(LspProcess {
        child,
        stdin: BufWriter::new(stdin),
        reader_task,
    })
}

async fn receive_response<R: LspRequest>(
    rx: oneshot::Receiver<Value>,
    timeout: Option<Duration>,
) -> Result<R::Result, ToolError>
where
    R::Result: DeserializeOwned,
{
    let response_value = match timeout {
        Some(duration) => tokio::time::timeout(duration, rx)
            .await
            .map_err(|_| execution_error("LSP response timed out"))?
            .map_err(|_| execution_error("LSP response channel closed"))?,
        None => rx
            .await
            .map_err(|_| execution_error("LSP response channel closed"))?,
    };
    let response: Response =
        serde_json::from_value(response_value).map_err(|source| ToolError::ExecutionFailed {
            source: Box::new(source),
        })?;

    if let Some(error) = response.error {
        return Err(execution_error(&format!(
            "LSP request failed with code {}: {}",
            error.code, error.message
        )));
    }

    let result_value = response.result.unwrap_or(Value::Null);

    serde_json::from_value(result_value).map_err(|source| ToolError::ExecutionFailed {
        source: Box::new(source),
    })
}

fn execution_error(message: &str) -> ToolError {
    ToolError::ExecutionFailed {
        source: Box::new(std::io::Error::other(message.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use dashmap::DashMap;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::sync::oneshot;

    use super::LspClient;

    #[tokio::test]
    async fn test_lsp_request_response_matching() {
        let pending = Arc::new(DashMap::<u32, oneshot::Sender<serde_json::Value>>::new());
        let (tx, rx) = oneshot::channel();
        pending.insert(7, tx);

        let response = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {
                "capabilities": {}
            }
        });

        if let Some((_, sender)) = pending.remove(&7) {
            let _ = sender.send(response.clone());
        }

        let received = rx.await.expect("test: response should be delivered");
        assert_eq!(received, response);
    }

    #[tokio::test]
    async fn test_lsp_lifecycle_initialize_shutdown() {
        let temp_dir = tempdir().expect("test: create temp dir");
        let script_path = temp_dir.path().join("mock-lsp.py");
        let log_path = temp_dir.path().join("lifecycle.log");
        let source_path = temp_dir.path().join("sample.rs");
        fs::write(&source_path, "fn main() {}\n").expect("test: write source file");

        write_executable_script(
            &script_path,
            &format!(
                r#"#!/usr/bin/env python3
import json
from pathlib import Path
import sys

log_path = Path({log_path:?})

def read_message():
    content_length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line == b"\r\n":
            break
        if line.lower().startswith(b"content-length:"):
            content_length = int(line.split(b":", 1)[1].strip())
    if content_length is None:
        return None
    body = sys.stdin.buffer.read(content_length)
    if not body:
        return None
    return json.loads(body.decode("utf-8"))

def send_message(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {{len(body)}}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    log_path.open("a", encoding="utf-8").write(message["method"] + "\n")
    if message["method"] == "initialize":
        send_message({{"jsonrpc": "2.0", "id": message["id"], "result": {{"capabilities": {{}} }} }})
    elif message["method"] == "shutdown":
        send_message({{"jsonrpc": "2.0", "id": message["id"], "result": None}})
    elif message["method"] == "exit":
        break
"#
            ),
        );

        let mut client = LspClient::spawn(
            script_path
                .to_str()
                .expect("test: script path should be valid utf-8"),
            temp_dir.path(),
        )
        .await
        .expect("test: spawn mock lsp client");

        client
            .initialize(temp_dir.path())
            .await
            .expect("test: initialize lsp client");
        assert!(client.is_ready());

        client
            .did_open(&source_path, "fn main() {}\n")
            .await
            .expect("test: didOpen notification should succeed");

        client.shutdown().await.expect("test: shutdown lsp client");
        assert!(!client.is_ready());

        tokio::time::sleep(Duration::from_millis(50)).await;

        let log = fs::read_to_string(&log_path).expect("test: read lifecycle log");
        assert!(log.contains("initialize"));
        assert!(log.contains("initialized"));
        assert!(log.contains("textDocument/didOpen"));
        assert!(log.contains("shutdown"));
        assert!(log.contains("exit"));
    }

    #[tokio::test]
    async fn test_lsp_crash_recovery_restarts() {
        let temp_dir = tempdir().expect("test: create temp dir");
        let script_path = temp_dir.path().join("restart-lsp.py");
        let counter_path = temp_dir.path().join("restart-count.txt");

        write_executable_script(
            &script_path,
            &format!(
                r#"#!/usr/bin/env python3
import json
from pathlib import Path
import sys

counter_path = Path({counter_path:?})
count = 0
if counter_path.exists():
    count = int(counter_path.read_text(encoding="utf-8"))
count += 1
counter_path.write_text(str(count), encoding="utf-8")

if count == 1:
    sys.exit(1)

def read_message():
    content_length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line == b"\r\n":
            break
        if line.lower().startswith(b"content-length:"):
            content_length = int(line.split(b":", 1)[1].strip())
    if content_length is None:
        return None
    body = sys.stdin.buffer.read(content_length)
    if not body:
        return None
    return json.loads(body.decode("utf-8"))

def send_message(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {{len(body)}}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    if message["method"] == "initialize":
        send_message({{"jsonrpc": "2.0", "id": message["id"], "result": {{"capabilities": {{}} }} }})
    elif message["method"] == "shutdown":
        send_message({{"jsonrpc": "2.0", "id": message["id"], "result": None}})
    elif message["method"] == "exit":
        break
"#
            ),
        );

        let mut client = LspClient::spawn(
            script_path
                .to_str()
                .expect("test: script path should be valid utf-8"),
            temp_dir.path(),
        )
        .await
        .expect("test: spawn restart mock lsp client");

        client
            .initialize(temp_dir.path())
            .await
            .expect("test: initialize after restart");
        assert!(client.is_ready());

        let attempts = fs::read_to_string(&counter_path).expect("test: read restart count");
        assert_eq!(attempts.trim(), "2");

        client
            .shutdown()
            .await
            .expect("test: shutdown after restart");
    }

    fn write_executable_script(path: &Path, contents: &str) {
        fs::write(path, contents).expect("test: write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(path).expect("test: stat script").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("test: chmod script");
        }
    }
}
