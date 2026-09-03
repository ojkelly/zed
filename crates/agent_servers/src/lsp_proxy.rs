//! The `_zed.dev/lsp` ACP extension: lets an agent see and query the language
//! servers Zed is already running for a session's project.
//!
//! Zed remains the language servers' only LSP client. Agents send requests and
//! receive the server's raw result; they never take part in the connection's
//! lifecycle, and never drive document synchronization. Zed owns `didOpen`,
//! `didChange`, `didClose` and `didSave`, and keeps servers current as buffers
//! change — including buffers the agent itself edits through `fs/write_text_file`
//! — so a forwarded request sees the same document state the user does.
//!
//! Access control is deliberately absent here: forwardable methods are the whole
//! client-to-server LSP surface, and the agent runtime applies its own permission
//! policy per method.

use agent_client_protocol::schema::v1 as acp;
use agent_client_protocol::{JsonRpcRequest, JsonRpcResponse};
use gpui::{App, AsyncApp, Entity};
use lsp::{
    DEFAULT_LSP_REQUEST_TIMEOUT, LanguageServer, request::Request as LspRequest,
};
use project::Project;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Key set to `true` in the client capabilities `_meta`, so agents can detect the
/// extension before calling it.
pub const CAPABILITY_KEY: &str = "lsp";

#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "_zed.dev/lsp/servers", response = ListServersResponse)]
#[serde(rename_all = "camelCase")]
pub struct ListServersRequest {
    pub session_id: acp::SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ListServersResponse {
    pub servers: Vec<LanguageServerInfo>,
    /// Every method `_zed.dev/lsp/request` will forward. Anything absent is
    /// rejected, so agents can filter their own tool surface against this rather
    /// than discovering the boundary through errors.
    pub available_methods: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageServerInfo {
    /// Identifies the server for `_zed.dev/lsp/request`, together with
    /// `workspacePath` when a project runs more than one server of this name.
    pub name: String,
    pub state: LanguageServerState,
    /// Root of the workspace this server was started for, distinguishing servers
    /// that share a name across several worktrees.
    pub workspace_path: PathBuf,
    /// File extensions (without the leading dot) of the language this server was
    /// started for, e.g. `["rs"]`. Empty if the language is unknown to Zed by the
    /// time of this call, which should not happen for a server that is `running`.
    pub file_extensions: Vec<String>,
    /// The LSP `languageId` for this server's language, e.g. `rust`. This is the
    /// value `textDocument.languageId` would carry in `didOpen` — useful context
    /// for interpreting results, even though the proxy sends `didOpen` itself and
    /// forwarded requests never need to supply it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_id: Option<String>,
    /// A human-readable server version, when the server reports one, e.g.
    /// `rust-analyzer 1.87.0`. Not machine-parseable; format is server-specific.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The server's `initialize` response capabilities, verbatim. Agents should
    /// consult these before sending a request rather than assuming support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageServerState {
    /// Initialized, with a process behind it, and able to answer requests.
    Running,
    /// Known to the project but not answering requests.
    ///
    /// Local servers never reach this: the project records a server's status only
    /// once it finishes starting, and drops that record when it stops, so one that
    /// is starting, stopped or restarting is absent from the listing rather than
    /// listed as this. That leaves servers owned by the host of a remote or
    /// collaborative project, which cannot be forwarded to.
    NotRunning,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "_zed.dev/lsp/request", response = SendRequestResponse)]
#[serde(rename_all = "camelCase")]
pub struct SendRequestRequest {
    pub session_id: acp::SessionId,
    /// A `name` from `_zed.dev/lsp/servers`, such as `rust-analyzer`.
    pub server_name: String,
    /// The `workspacePath` of the intended entry in `_zed.dev/lsp/servers`.
    ///
    /// Required, because a name alone is ambiguous: a project with more than one
    /// worktree of the same language runs one server per worktree.
    pub workspace_path: PathBuf,
    /// An LSP client-to-server method name, such as `textDocument/hover`.
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct SendRequestResponse {
    /// The server's result, deserialized and re-serialized but not reshaped.
    pub result: Value,
}

/// Generates the forwarding table from `lsp_types` request types.
///
/// Method names come from each type's `METHOD` constant rather than being
/// written out again, so the table cannot drift from the protocol definitions.
macro_rules! forwardable_requests {
    ($($request:ty),+ $(,)?) => {
        /// Every method this extension will forward, for diagnostics and docs.
        pub fn available_methods() -> Vec<&'static str> {
            vec![$(<$request as LspRequest>::METHOD),+]
        }

        async fn forward(
            server: Arc<LanguageServer>,
            method: &str,
            params: Value,
        ) -> Result<Value, acp::Error> {
            $(
                if method == <$request as LspRequest>::METHOD {
                    let params = serde_json::from_value::<<$request as LspRequest>::Params>(params)
                        .map_err(|err| {
                            acp::Error::invalid_params()
                                .data(format!("params for {method} are not valid: {err}"))
                        })?;
                    let result = server
                        .request::<$request>(params, DEFAULT_LSP_REQUEST_TIMEOUT)
                        .await
                        .into_response()
                        .map_err(|err| {
                            acp::Error::internal_error()
                                .data(format!("{method} failed: {err:#}"))
                        })?;
                    return serde_json::to_value(result).map_err(|err| {
                        acp::Error::internal_error()
                            .data(format!("could not serialize the {method} result: {err}"))
                    });
                }
            )+
            Err(acp::Error::method_not_found().data(format!(
                "{method} cannot be forwarded: it is not a client-to-server LSP request, \
                 or it belongs to document synchronization, which Zed owns"
            )))
        }
    };
}

// The client-to-server LSP surface, minus `initialize`/`shutdown` and the
// document lifecycle. Server-to-client requests are absent because Zed, not the
// agent, is the client: those arrive at Zed and are answered there.
forwardable_requests![
    lsp::request::CallHierarchyIncomingCalls,
    lsp::request::CallHierarchyOutgoingCalls,
    lsp::request::CallHierarchyPrepare,
    lsp::request::CodeActionRequest,
    lsp::request::CodeActionResolveRequest,
    lsp::request::CodeLensRequest,
    lsp::request::CodeLensResolve,
    lsp::request::ColorPresentationRequest,
    lsp::request::Completion,
    lsp::request::DocumentColor,
    lsp::request::DocumentDiagnosticRequest,
    lsp::request::DocumentHighlightRequest,
    lsp::request::DocumentLinkRequest,
    lsp::request::DocumentLinkResolve,
    lsp::request::DocumentSymbolRequest,
    lsp::request::ExecuteCommand,
    lsp::request::Formatting,
    lsp::request::GotoDeclaration,
    lsp::request::GotoDefinition,
    lsp::request::GotoImplementation,
    lsp::request::GotoTypeDefinition,
    lsp::request::HoverRequest,
    lsp::request::InlayHintRequest,
    lsp::request::InlayHintResolveRequest,
    lsp::request::InlineValueRequest,
    lsp::request::MonikerRequest,
    lsp::request::PrepareRenameRequest,
    lsp::request::RangeFormatting,
    lsp::request::References,
    lsp::request::Rename,
    lsp::request::SemanticTokensFullDeltaRequest,
    lsp::request::SemanticTokensFullRequest,
    lsp::request::SemanticTokensRangeRequest,
    lsp::request::SignatureHelpRequest,
    lsp::request::ResolveCompletionItem,
    lsp::request::TypeHierarchyPrepare,
    lsp::request::TypeHierarchySubtypes,
    lsp::request::TypeHierarchySupertypes,
    lsp::request::WorkspaceDiagnosticRequest,
    lsp::request::WorkspaceSymbolRequest,
    lsp::request::WorkspaceSymbolResolve,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_the_client_to_server_surface() {
        let methods = available_methods();

        for expected in [
            "textDocument/hover",
            "textDocument/definition",
            "textDocument/references",
            "textDocument/rename",
            "textDocument/diagnostic",
            // Allowed deliberately: side effects are the agent runtime's to gate,
            // not ours to pre-empt.
            "workspace/executeCommand",
        ] {
            assert!(
                methods.contains(&expected),
                "{expected} should be forwardable"
            );
        }
    }

    #[test]
    fn withholds_lifecycle_and_document_synchronization() {
        let methods = available_methods();

        // Zed is the client: these would either tear down a connection shared with
        // the user's editor, or desynchronize documents Zed is responsible for.
        for withheld in [
            "initialize",
            "shutdown",
            "exit",
            "textDocument/didOpen",
            "textDocument/didChange",
            "textDocument/didClose",
            "textDocument/didSave",
            "textDocument/willSaveWaitUntil",
            "workspace/didChangeConfiguration",
            "workspace/didChangeWatchedFiles",
            "$/cancelRequest",
        ] {
            assert!(
                !methods.contains(&withheld),
                "{withheld} should not be forwardable"
            );
        }
    }

    #[test]
    fn withholds_requests_only_a_server_may_send() {
        let methods = available_methods();

        for withheld in [
            "workspace/applyEdit",
            "workspace/configuration",
            "client/registerCapability",
            "window/showMessageRequest",
            "window/workDoneProgress/create",
        ] {
            assert!(
                !methods.contains(&withheld),
                "{withheld} is sent by a server to its client, not forwardable"
            );
        }
    }
}

pub fn list_servers(project: &Entity<Project>, cx: &App) -> ListServersResponse {
    let project = project.read(cx);
    let lsp_store = project.lsp_store().read(cx);

    let servers = lsp_store
        .language_server_statuses()
        // A server with no workspace is one the host of a remote or collaborative
        // project owns. It cannot be addressed, so listing it would only offer the
        // agent a server every request against it would reject.
        .filter_map(|(server_id, status)| {
            let workspace_path = server_workspace_path(project, status, cx)?;
            let running = lsp_store.language_server_for_id(server_id).is_some();
            let language = status.language_name.as_ref().and_then(|language_name| {
                project
                    .languages()
                    .to_vec()
                    .into_iter()
                    .find(|language| &language.name() == language_name)
            });
            Some(LanguageServerInfo {
                name: status.name.to_string(),
                state: if running {
                    LanguageServerState::Running
                } else {
                    LanguageServerState::NotRunning
                },
                workspace_path,
                file_extensions: language
                    .as_ref()
                    .map(|language| language.path_suffixes().to_vec())
                    .unwrap_or_default(),
                language_id: language.as_ref().map(|language| {
                    project
                        .languages()
                        .lsp_adapters(&language.name())
                        .into_iter()
                        .find(|adapter| adapter.name().0 == status.name.0)
                        .map(|adapter| adapter.language_id(&language.name()))
                        .unwrap_or_else(|| language.name().lsp_id())
                }),
                version: status.server_readable_version.as_ref().map(ToString::to_string),
                capabilities: lsp_store
                    .lsp_server_capabilities
                    .get(&server_id)
                    .and_then(|capabilities| serde_json::to_value(capabilities).ok()),
            })
        })
        .collect();

    ListServersResponse {
        servers,
        available_methods: available_methods()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    }
}

pub async fn send_request(
    project: Entity<Project>,
    request: SendRequestRequest,
    cx: &mut AsyncApp,
) -> Result<SendRequestResponse, acp::Error> {
    // Hold the buffer open for the duration of the request. A server only answers
    // `textDocument/*` for documents it has been told about, and Zed registers a
    // buffer with its servers only while something holds it open.
    let _open_buffer = open_referenced_buffer(&project, &request.params, cx).await?;

    let server = project.update(cx, |project, cx| {
        resolve_server(project, &request.server_name, &request.workspace_path, cx)
    })?;

    let result = forward(server, &request.method, request.params).await?;
    Ok(SendRequestResponse { result })
}

/// Finds the running server identified by `name` and `workspace_path`.
///
/// Servers are addressed by that pair rather than by Zed's internal id, which does
/// not survive a server restart, and by more than a name, which is ambiguous when a
/// project has several worktrees of the same language.
fn resolve_server(
    project: &Project,
    name: &str,
    workspace_path: &Path,
    cx: &App,
) -> Result<Arc<LanguageServer>, acp::Error> {
    let lsp_store = project.lsp_store().read(cx);

    lsp_store
        .language_server_statuses()
        .filter(|(_, status)| status.name.0 == name)
        .find_map(|(server_id, status)| {
            if server_workspace_path(project, status, cx).as_deref() != Some(workspace_path) {
                return None;
            }
            lsp_store.language_server_for_id(server_id)
        })
        .ok_or_else(|| {
            acp::Error::invalid_params().data(format!(
                "no running language server named {name} for workspace {}; \
                 call _zed.dev/lsp/servers for the current set",
                workspace_path.display()
            ))
        })
}

fn server_workspace_path(
    project: &Project,
    status: &project::LanguageServerStatus,
    cx: &App,
) -> Option<PathBuf> {
    let worktree_id = status.worktree?;
    Some(
        project
            .worktree_for_id(worktree_id, cx)?
            .read(cx)
            .abs_path()
            .to_path_buf(),
    )
}

/// Opens the buffer named by `textDocument.uri`, if the params name one.
///
/// Absent, unparseable or out-of-project URIs are not an error: the request is
/// forwarded anyway, and the server decides what to make of it. Registration is
/// also skipped by the project for files it cannot serve, such as those outside a
/// worktree, so a successful open does not guarantee the server knows the
/// document.
async fn open_referenced_buffer(
    project: &Entity<Project>,
    params: &Value,
    cx: &mut AsyncApp,
) -> Result<Option<project::lsp_store::OpenLspBufferHandle>, acp::Error> {
    let Some(document) = params
        .get("textDocument")
        .map(|document| serde_json::from_value::<lsp::TextDocumentIdentifier>(document.clone()))
        .transpose()
        .ok()
        .flatten()
    else {
        return Ok(None);
    };
    let Ok(abs_path) = document.uri.to_file_path() else {
        return Ok(None);
    };

    let open = project.update(cx, |project, cx| {
        let path = project.project_path_for_absolute_path(&abs_path, cx)?;
        Some(project.open_buffer(path, cx))
    });

    let Some(open) = open else {
        return Ok(None);
    };

    let buffer = open.await.map_err(|err| {
        acp::Error::internal_error().data(format!("could not open {}: {err:#}", abs_path.display()))
    })?;

    Ok(Some(project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&buffer, cx)
    })))
}
