use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lsp_types::notification::{Notification as LspNotification, PublishDiagnostics};
use lsp_types::request::{
    DocumentSymbolRequest, GotoDefinition, PrepareRenameRequest, References, Rename,
    WorkspaceSymbolRequest,
};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentChangeOperation, DocumentChanges, DocumentSymbol,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    Location, LocationLink, OneOf, PartialResultParams, Position, PrepareRenameResponse,
    PublishDiagnosticsParams, Range, ReferenceContext, ReferenceParams, RenameParams,
    SymbolInformation, SymbolKind, TextDocumentIdentifier, TextDocumentPositionParams, TextEdit,
    Uri, WorkDoneProgressParams, WorkspaceEdit, WorkspaceSymbol, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::fs;
use tokio::sync::broadcast;
use url::Url;

use super::client::LspClient;
use crate::tool::ToolError;

const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(5);

pub struct LspGotoDefinitionTool<'a> {
    client: &'a LspClient,
}

impl<'a> LspGotoDefinitionTool<'a> {
    pub fn new(client: &'a LspClient) -> Self {
        Self { client }
    }

    pub async fn run(&self, path: &Path, line: u32, character: u32) -> Result<String, ToolError> {
        ensure_ready(self.client)?;
        let params = GotoDefinitionParams {
            text_document_position_params: text_document_position(path, line, character)?,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let response = self.client.request::<GotoDefinition>(params).await?;
        let formatted = format_goto_definition_response(response)?;
        Ok(join_or_empty(formatted, "No definition found"))
    }
}

pub struct LspFindReferencesTool<'a> {
    client: &'a LspClient,
}

impl<'a> LspFindReferencesTool<'a> {
    pub fn new(client: &'a LspClient) -> Self {
        Self { client }
    }

    pub async fn run(&self, path: &Path, line: u32, character: u32) -> Result<String, ToolError> {
        ensure_ready(self.client)?;
        let params = ReferenceParams {
            text_document_position: text_document_position(path, line, character)?,
            context: ReferenceContext {
                include_declaration: true,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let response = self.client.request::<References>(params).await?;
        let formatted = response
            .unwrap_or_default()
            .iter()
            .map(location_to_string)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(join_or_empty(formatted, "No references found"))
    }
}

pub struct LspDiagnosticsTool<'a> {
    client: &'a LspClient,
}

impl<'a> LspDiagnosticsTool<'a> {
    pub fn new(client: &'a LspClient) -> Self {
        Self { client }
    }

    pub async fn run(
        &self,
        path: &Path,
        severity_filter: Option<DiagnosticSeverity>,
    ) -> Result<String, ToolError> {
        ensure_ready(self.client)?;

        let content = fs::read_to_string(path).await.map_err(wrap_io_error)?;
        self.client.did_open(path, &content).await?;

        let target_uri = file_uri(path)?;
        let mut notification_rx = self.client.notification_rx();
        let params = wait_for_publish_diagnostics(&mut notification_rx, &target_uri).await?;
        let formatted = format_publish_diagnostics(&params, severity_filter)?;
        Ok(join_or_empty(formatted, "No diagnostics found"))
    }
}

pub struct LspSymbolsTool<'a> {
    client: &'a LspClient,
}

impl<'a> LspSymbolsTool<'a> {
    pub fn new(client: &'a LspClient) -> Self {
        Self { client }
    }

    pub async fn run(&self, path: &Path, query: Option<&str>) -> Result<String, ToolError> {
        ensure_ready(self.client)?;

        let formatted = match query.filter(|value| !value.trim().is_empty()) {
            Some(query) => {
                let params = WorkspaceSymbolParams {
                    query: query.to_string(),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                };
                format_workspace_symbol_response(
                    self.client
                        .request::<WorkspaceSymbolRequest>(params)
                        .await?,
                )?
            }
            None => {
                let params = DocumentSymbolParams {
                    text_document: text_document(path)?,
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                };
                format_document_symbol_response(
                    path,
                    self.client.request::<DocumentSymbolRequest>(params).await?,
                )?
            }
        };

        Ok(join_or_empty(formatted, "No symbols found"))
    }
}

pub struct LspRenameTool<'a> {
    client: &'a LspClient,
}

impl<'a> LspRenameTool<'a> {
    pub fn new(client: &'a LspClient) -> Self {
        Self { client }
    }

    pub async fn run(
        &self,
        path: &Path,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<String, ToolError> {
        ensure_ready(self.client)?;

        let position = text_document_position(path, line, character)?;
        let prepare = self
            .client
            .request::<PrepareRenameRequest>(position.clone())
            .await?
            .ok_or_else(|| {
                execution_error("LSP server rejected rename at the requested position")
            })?;
        let old_name = extract_prepared_symbol_name(path, &prepare).await?;

        let rename_params = RenameParams {
            text_document_position: position,
            new_name: new_name.to_string(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let edit = self
            .client
            .request::<Rename>(rename_params)
            .await?
            .unwrap_or_default();
        let file_changes = apply_workspace_edit(&edit).await?;

        Ok(format!(
            "Renamed {old_name} to {new_name} in {file_changes} files"
        ))
    }
}

fn ensure_ready(client: &LspClient) -> Result<(), ToolError> {
    if client.is_ready() {
        Ok(())
    } else {
        Err(execution_error("LSP client is not initialized"))
    }
}

fn execution_error(message: &str) -> ToolError {
    ToolError::ExecutionFailed {
        source: Box::new(std::io::Error::other(message.to_string())),
    }
}

fn wrap_io_error(source: std::io::Error) -> ToolError {
    ToolError::ExecutionFailed {
        source: Box::new(source),
    }
}

fn text_document(path: &Path) -> Result<TextDocumentIdentifier, ToolError> {
    Ok(TextDocumentIdentifier::new(file_uri(path)?))
}

fn text_document_position(
    path: &Path,
    line: u32,
    character: u32,
) -> Result<TextDocumentPositionParams, ToolError> {
    if line == 0 {
        return Err(execution_error("line numbers must be 1-based"));
    }

    Ok(TextDocumentPositionParams::new(
        text_document(path)?,
        Position {
            line: line - 1,
            character,
        },
    ))
}

fn file_uri(path: &Path) -> Result<Uri, ToolError> {
    let url = Url::from_file_path(path).map_err(|_| {
        execution_error(&format!(
            "failed to convert path to file URL: {}",
            path.display()
        ))
    })?;
    url.as_str().parse().map_err(|_| {
        execution_error(&format!(
            "failed to convert URL to LSP URI: {}",
            path.display()
        ))
    })
}

fn file_path_string(uri: &Uri) -> Result<String, ToolError> {
    let url = Url::parse(uri.as_str())
        .map_err(|_| execution_error(&format!("failed to parse URI as URL: {}", uri.as_str())))?;
    let path = url.to_file_path().map_err(|_| {
        execution_error(&format!(
            "failed to convert URI to file path: {}",
            uri.as_str()
        ))
    })?;
    Ok(path.display().to_string())
}

fn position_string(path: &str, position: Position) -> String {
    format!(
        "{path}:{}:{}",
        position.line.saturating_add(1),
        position.character.saturating_add(1)
    )
}

fn range_string(path: &str, range: Range) -> String {
    format!(
        "{path}:{}-{}",
        range.start.line.saturating_add(1),
        range.end.line.saturating_add(1)
    )
}

fn location_to_string(location: &Location) -> Result<String, ToolError> {
    let path = file_path_string(&location.uri)?;
    Ok(position_string(&path, location.range.start))
}

fn location_link_to_string(location: &LocationLink) -> Result<String, ToolError> {
    let path = file_path_string(&location.target_uri)?;
    Ok(position_string(
        &path,
        location.target_selection_range.start,
    ))
}

fn format_goto_definition_response(
    response: Option<GotoDefinitionResponse>,
) -> Result<Vec<String>, ToolError> {
    match response {
        None => Ok(Vec::new()),
        Some(GotoDefinitionResponse::Scalar(location)) => Ok(vec![location_to_string(&location)?]),
        Some(GotoDefinitionResponse::Array(locations)) => locations
            .iter()
            .map(location_to_string)
            .collect::<Result<Vec<_>, _>>(),
        Some(GotoDefinitionResponse::Link(links)) => links
            .iter()
            .map(location_link_to_string)
            .collect::<Result<Vec<_>, _>>(),
    }
}

#[derive(Deserialize)]
struct NotificationEnvelope {
    method: String,
    params: Option<Value>,
}

fn parse_publish_diagnostics_notification(
    value: Value,
) -> Result<Option<PublishDiagnosticsParams>, ToolError> {
    let notification: NotificationEnvelope =
        serde_json::from_value(value).map_err(|source| ToolError::ExecutionFailed {
            source: Box::new(source),
        })?;

    if notification.method != PublishDiagnostics::METHOD {
        return Ok(None);
    }

    let params = notification
        .params
        .ok_or_else(|| execution_error("publishDiagnostics notification missing params"))?;
    serde_json::from_value(params)
        .map(Some)
        .map_err(|source| ToolError::ExecutionFailed {
            source: Box::new(source),
        })
}

async fn wait_for_publish_diagnostics(
    notification_rx: &mut broadcast::Receiver<Value>,
    target_uri: &Uri,
) -> Result<PublishDiagnosticsParams, ToolError> {
    let deadline = tokio::time::sleep(DIAGNOSTICS_TIMEOUT);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => {
                return Err(execution_error("timed out waiting for publishDiagnostics notification"));
            }
            message = notification_rx.recv() => {
                match message {
                    Ok(value) => {
                        if let Some(params) = parse_publish_diagnostics_notification(value)? {
                            if params.uri == *target_uri {
                                return Ok(params);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(execution_error("LSP notification channel closed"));
                    }
                }
            }
        }
    }
}

fn format_publish_diagnostics(
    params: &PublishDiagnosticsParams,
    severity_filter: Option<DiagnosticSeverity>,
) -> Result<Vec<String>, ToolError> {
    let path = file_path_string(&params.uri)?;
    Ok(params
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            severity_filter.is_none_or(|filter| diagnostic.severity == Some(filter))
        })
        .map(|diagnostic| format_diagnostic(&path, diagnostic))
        .collect())
}

fn format_diagnostic(path: &str, diagnostic: &Diagnostic) -> String {
    let severity = diagnostic
        .severity
        .map(diagnostic_severity_name)
        .unwrap_or("INFO");
    format!(
        "{}: [{}] {}",
        position_string(path, diagnostic.range.start),
        severity,
        diagnostic.message
    )
}

fn diagnostic_severity_name(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::ERROR => "ERROR",
        DiagnosticSeverity::WARNING => "WARN",
        DiagnosticSeverity::INFORMATION => "INFO",
        DiagnosticSeverity::HINT => "HINT",
        _ => "INFO",
    }
}

fn format_document_symbol_response(
    path: &Path,
    response: Option<DocumentSymbolResponse>,
) -> Result<Vec<String>, ToolError> {
    let path = path.display().to_string();
    match response {
        None => Ok(Vec::new()),
        Some(DocumentSymbolResponse::Flat(symbols)) => Ok(symbols
            .iter()
            .map(format_symbol_information)
            .collect::<Result<Vec<_>, _>>()?),
        Some(DocumentSymbolResponse::Nested(symbols)) => {
            let mut formatted = Vec::new();
            for symbol in &symbols {
                collect_document_symbols(&path, symbol, &mut formatted);
            }
            Ok(formatted)
        }
    }
}

fn collect_document_symbols(path: &str, symbol: &DocumentSymbol, out: &mut Vec<String>) {
    out.push(format!(
        "{} {} — {}",
        symbol_kind_name(symbol.kind),
        symbol.name,
        range_string(path, symbol.selection_range)
    ));
    if let Some(children) = &symbol.children {
        for child in children {
            collect_document_symbols(path, child, out);
        }
    }
}

fn format_workspace_symbol_response(
    response: Option<WorkspaceSymbolResponse>,
) -> Result<Vec<String>, ToolError> {
    match response {
        None => Ok(Vec::new()),
        Some(WorkspaceSymbolResponse::Flat(symbols)) => symbols
            .iter()
            .map(format_symbol_information)
            .collect::<Result<Vec<_>, _>>(),
        Some(WorkspaceSymbolResponse::Nested(symbols)) => symbols
            .iter()
            .map(format_workspace_symbol)
            .collect::<Result<Vec<_>, _>>(),
    }
}

fn format_symbol_information(symbol: &SymbolInformation) -> Result<String, ToolError> {
    let path = file_path_string(&symbol.location.uri)?;
    Ok(format!(
        "{} {} — {}",
        symbol_kind_name(symbol.kind),
        symbol.name,
        range_string(&path, symbol.location.range)
    ))
}

fn format_workspace_symbol(symbol: &WorkspaceSymbol) -> Result<String, ToolError> {
    let location = match &symbol.location {
        OneOf::Left(location) => {
            let path = file_path_string(&location.uri)?;
            range_string(&path, location.range)
        }
        OneOf::Right(location) => file_path_string(&location.uri)?,
    };

    Ok(format!(
        "{} {} — {}",
        symbol_kind_name(symbol.kind),
        symbol.name,
        location
    ))
}

fn symbol_kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::FILE => "file",
        SymbolKind::MODULE => "module",
        SymbolKind::NAMESPACE => "namespace",
        SymbolKind::PACKAGE => "package",
        SymbolKind::CLASS => "class",
        SymbolKind::METHOD => "method",
        SymbolKind::PROPERTY => "property",
        SymbolKind::FIELD => "field",
        SymbolKind::CONSTRUCTOR => "constructor",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "interface",
        SymbolKind::FUNCTION => "function",
        SymbolKind::VARIABLE => "variable",
        SymbolKind::CONSTANT => "constant",
        SymbolKind::STRING => "string",
        SymbolKind::NUMBER => "number",
        SymbolKind::BOOLEAN => "boolean",
        SymbolKind::ARRAY => "array",
        SymbolKind::OBJECT => "object",
        SymbolKind::KEY => "key",
        SymbolKind::NULL => "null",
        SymbolKind::ENUM_MEMBER => "enum_member",
        SymbolKind::STRUCT => "struct",
        SymbolKind::EVENT => "event",
        SymbolKind::OPERATOR => "operator",
        SymbolKind::TYPE_PARAMETER => "type_parameter",
        _ => "symbol",
    }
}

async fn extract_prepared_symbol_name(
    path: &Path,
    response: &PrepareRenameResponse,
) -> Result<String, ToolError> {
    match response {
        PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. } => Ok(placeholder.clone()),
        PrepareRenameResponse::Range(range) => {
            let content = fs::read_to_string(path).await.map_err(wrap_io_error)?;
            extract_range_text(&content, *range)
        }
        PrepareRenameResponse::DefaultBehavior { .. } => Ok("symbol".to_string()),
    }
}

fn extract_range_text(content: &str, range: Range) -> Result<String, ToolError> {
    let start = position_to_byte_offset(content, range.start)?;
    let end = position_to_byte_offset(content, range.end)?;
    content
        .get(start..end)
        .map(ToOwned::to_owned)
        .ok_or_else(|| execution_error("failed to extract prepared rename range from file content"))
}

async fn apply_workspace_edit(edit: &WorkspaceEdit) -> Result<usize, ToolError> {
    let grouped = workspace_edit_changes(edit)?;

    for (path, edits) in &grouped {
        apply_text_edits(path, edits).await?;
    }

    Ok(grouped.len())
}

fn workspace_edit_changes(
    edit: &WorkspaceEdit,
) -> Result<HashMap<PathBuf, Vec<TextEdit>>, ToolError> {
    let mut grouped: HashMap<PathBuf, Vec<TextEdit>> = HashMap::new();

    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            let path = uri_to_path_buf(uri)?;
            grouped
                .entry(path)
                .or_default()
                .extend(edits.iter().cloned());
        }
    }

    if let Some(document_changes) = &edit.document_changes {
        match document_changes {
            DocumentChanges::Edits(edits) => {
                for edit in edits {
                    let path = uri_to_path_buf(&edit.text_document.uri)?;
                    let file_edits = edit.edits.iter().map(one_of_text_edit).collect::<Vec<_>>();
                    grouped.entry(path).or_default().extend(file_edits);
                }
            }
            DocumentChanges::Operations(operations) => {
                for operation in operations {
                    match operation {
                        DocumentChangeOperation::Edit(edit) => {
                            let path = uri_to_path_buf(&edit.text_document.uri)?;
                            let file_edits =
                                edit.edits.iter().map(one_of_text_edit).collect::<Vec<_>>();
                            grouped.entry(path).or_default().extend(file_edits);
                        }
                        DocumentChangeOperation::Op(_) => {
                            return Err(execution_error(
                                "workspace edit contains unsupported resource operations",
                            ));
                        }
                    }
                }
            }
        }
    }

    Ok(grouped)
}

fn one_of_text_edit(edit: &OneOf<TextEdit, lsp_types::AnnotatedTextEdit>) -> TextEdit {
    match edit {
        OneOf::Left(edit) => edit.clone(),
        OneOf::Right(edit) => edit.text_edit.clone(),
    }
}

fn uri_to_path_buf(uri: &Uri) -> Result<PathBuf, ToolError> {
    let url = Url::parse(uri.as_str())
        .map_err(|_| execution_error(&format!("failed to parse URI as URL: {}", uri.as_str())))?;
    url.to_file_path().map_err(|_| {
        execution_error(&format!(
            "failed to convert URI to file path: {}",
            uri.as_str()
        ))
    })
}

async fn apply_text_edits(path: &Path, edits: &[TextEdit]) -> Result<(), ToolError> {
    let mut content = fs::read_to_string(path).await.map_err(wrap_io_error)?;
    let mut ordered = edits.to_vec();
    ordered.sort_by(|left, right| {
        right
            .range
            .start
            .line
            .cmp(&left.range.start.line)
            .then_with(|| right.range.start.character.cmp(&left.range.start.character))
    });

    for edit in ordered {
        let start = position_to_byte_offset(&content, edit.range.start)?;
        let end = position_to_byte_offset(&content, edit.range.end)?;
        content.replace_range(start..end, &edit.new_text);
    }

    fs::write(path, content).await.map_err(wrap_io_error)
}

fn position_to_byte_offset(content: &str, position: Position) -> Result<usize, ToolError> {
    let mut line = 0_u32;
    let mut utf16_col = 0_u32;

    for (index, ch) in content.char_indices() {
        if line == position.line && utf16_col == position.character {
            return Ok(index);
        }

        if ch == '\n' {
            if line == position.line {
                return Ok(index);
            }
            line += 1;
            utf16_col = 0;
            continue;
        }

        if line == position.line {
            utf16_col += ch.len_utf16() as u32;
            if utf16_col == position.character {
                return Ok(index + ch.len_utf8());
            }
        }
    }

    if line == position.line && utf16_col == position.character {
        Ok(content.len())
    } else {
        Err(execution_error("LSP position is outside file bounds"))
    }
}

fn join_or_empty(lines: Vec<String>, empty_message: &str) -> String {
    if lines.is_empty() {
        empty_message.to_string()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod lsp_tools {
    use super::*;
    use lsp_types::{DiagnosticSeverity, WorkspaceEdit};
    use serde_json::json;

    #[test]
    fn test_lsp_goto_definition_response_parsing() {
        let response: Option<GotoDefinitionResponse> = serde_json::from_value(json!({
            "uri": "file:///workspace/src/lib.rs",
            "range": {
                "start": { "line": 9, "character": 4 },
                "end": { "line": 9, "character": 12 }
            }
        }))
        .expect("test: parse goto definition response");

        let formatted =
            format_goto_definition_response(response).expect("test: format goto response");
        assert_eq!(formatted, vec!["/workspace/src/lib.rs:10:5"]);
    }

    #[test]
    fn test_lsp_diagnostics_notification_parsing() {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///workspace/src/lib.rs",
                "diagnostics": [
                    {
                        "range": {
                            "start": { "line": 2, "character": 7 },
                            "end": { "line": 2, "character": 12 }
                        },
                        "severity": 1,
                        "message": "cannot find value `thing` in this scope"
                    },
                    {
                        "range": {
                            "start": { "line": 4, "character": 0 },
                            "end": { "line": 4, "character": 3 }
                        },
                        "severity": 4,
                        "message": "unused variable"
                    }
                ]
            }
        });

        let params = parse_publish_diagnostics_notification(notification)
            .expect("test: parse notification")
            .expect("test: should be publishDiagnostics");
        let formatted = format_publish_diagnostics(&params, Some(DiagnosticSeverity::ERROR))
            .expect("test: format diagnostics");

        assert_eq!(
            formatted,
            vec!["/workspace/src/lib.rs:3:8: [ERROR] cannot find value `thing` in this scope"]
        );
    }

    #[test]
    fn test_lsp_rename_workspace_edit_parsing() {
        let edit: WorkspaceEdit = serde_json::from_value(json!({
            "changes": {
                "file:///workspace/src/lib.rs": [
                    {
                        "range": {
                            "start": { "line": 1, "character": 3 },
                            "end": { "line": 1, "character": 8 }
                        },
                        "newText": "better_name"
                    }
                ]
            },
            "documentChanges": [
                {
                    "textDocument": {
                        "uri": "file:///workspace/src/main.rs",
                        "version": null
                    },
                    "edits": [
                        {
                            "range": {
                                "start": { "line": 5, "character": 1 },
                                "end": { "line": 5, "character": 6 }
                            },
                            "newText": "better_name"
                        }
                    ]
                }
            ]
        }))
        .expect("test: parse workspace edit");

        let grouped = workspace_edit_changes(&edit).expect("test: group workspace edits");
        assert_eq!(grouped.len(), 2);
        assert_eq!(
            grouped
                .get(&PathBuf::from("/workspace/src/lib.rs"))
                .expect("test: lib.rs edits")
                .len(),
            1
        );
        assert_eq!(
            grouped
                .get(&PathBuf::from("/workspace/src/main.rs"))
                .expect("test: main.rs edits")
                .len(),
            1
        );
    }
}
