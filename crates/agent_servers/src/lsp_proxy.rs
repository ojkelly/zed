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
use project::{Project, WorktreeId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Key holding the extension's capability object in the client capabilities
/// `_meta`, so agents can detect the extension before calling it. Absent or
/// `null` means unsupported; any object, including an empty one, means
/// supported — the same convention ACP uses for `elicitation`.
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
    /// Identifies this server for `_zed.dev/lsp/request`. Opaque: callers must
    /// not parse it. Stable across a restart of this same server, since it is
    /// derived from `name` and `workspacePath`, neither of which changes when a
    /// server restarts.
    pub server_id: String,
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
    /// Spawned but not yet initialized. `_zed.dev/lsp/request` against this
    /// `serverId` fails until the server finishes starting; the id remains valid
    /// once it does, since it does not change across that transition.
    Starting,
    /// Known to the project but not answering requests, and not `starting`: a
    /// server owned by the host of a remote or collaborative project, which
    /// cannot be forwarded to. A local server that has merely stopped is absent
    /// from the listing rather than appearing here; a local restart passes back
    /// through `Starting` once the new process spawns.
    NotRunning,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "_zed.dev/lsp/request", response = SendRequestResponse)]
#[serde(rename_all = "camelCase")]
pub struct SendRequestRequest {
    pub session_id: acp::SessionId,
    /// A `serverId` from `_zed.dev/lsp/servers`.
    pub server_id: String,
    /// An LSP client-to-server method name, such as `textDocument/hover`.
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct SendRequestResponse {
    /// The server's result, deserialized and re-serialized but not reshaped.
    pub lsp_result: Value,
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

/// Mints the opaque, restart-stable id addressing a server in
/// `_zed.dev/lsp/request`.
///
/// Callers must treat this as opaque and never parse it; it happens to be
/// derived deterministically from `name` and `workspace_path`, joined on a NUL
/// byte (which cannot appear in either), purely so this side can recompute and
/// match it without maintaining a separate id table. It is stable across a
/// restart because neither `name` nor `workspace_path` changes when a server
/// restarts.
fn make_server_id(name: &str, workspace_path: &Path) -> String {
    format!("{name}\0{}", workspace_path.display())
}

pub fn list_servers(project: &Entity<Project>, cx: &App) -> ListServersResponse {
    let project = project.read(cx);
    let lsp_store = project.lsp_store().read(cx);

    let running = lsp_store
        .language_server_statuses()
        // A server with no workspace is one the host of a remote or collaborative
        // project owns. It cannot be addressed, so listing it would only offer the
        // agent a server every request against it would reject.
        .filter_map(|(server_id, status)| {
            let workspace_path = server_workspace_path(project, status.worktree, cx)?;
            let is_running = lsp_store.language_server_for_id(server_id).is_some();
            let language = status.language_name.as_ref().and_then(|language_name| {
                project
                    .languages()
                    .to_vec()
                    .into_iter()
                    .find(|language| &language.name() == language_name)
            });
            Some(LanguageServerInfo {
                server_id: make_server_id(&status.name.0, &workspace_path),
                name: status.name.to_string(),
                state: if is_running {
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
        });

    let starting = lsp_store
        .starting_language_servers()
        .into_iter()
        .filter_map(|(_server_id, name, worktree_id)| {
            let workspace_path = server_workspace_path(project, Some(worktree_id), cx)?;
            Some(LanguageServerInfo {
                server_id: make_server_id(&name.0, &workspace_path),
                name: name.to_string(),
                state: LanguageServerState::Starting,
                workspace_path,
                // Not yet known: the language and its adapter aren't resolved
                // until the server finishes starting.
                file_extensions: Vec::new(),
                language_id: None,
                version: None,
                capabilities: None,
            })
        });

    let servers = running.chain(starting).collect();

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

    let server = project.update(cx, |project, cx| resolve_server(project, &request.server_id, cx))?;

    let lsp_result = forward(server, &request.method, request.params).await?;
    Ok(SendRequestResponse { lsp_result })
}

/// Finds the running server whose minted id matches `server_id`.
///
/// The id is never parsed back apart — every candidate server's id is
/// recomputed from its own `(name, workspace_path)` and compared, so this side
/// never has to trust or decode caller-supplied structure, only equality.
fn resolve_server(
    project: &Project,
    server_id: &str,
    cx: &App,
) -> Result<Arc<LanguageServer>, acp::Error> {
    let lsp_store = project.lsp_store().read(cx);

    let running = lsp_store.language_server_statuses().find_map(|(id, status)| {
        let workspace_path = server_workspace_path(project, status.worktree, cx)?;
        (make_server_id(&status.name.0, &workspace_path) == server_id)
            .then(|| lsp_store.language_server_for_id(id))
            .flatten()
    });
    if let Some(server) = running {
        return Ok(server);
    }

    let starting = lsp_store
        .starting_language_servers()
        .into_iter()
        .find(|(_, name, worktree_id)| {
            server_workspace_path(project, Some(*worktree_id), cx)
                .is_some_and(|workspace_path| make_server_id(&name.0, &workspace_path) == server_id)
        });
    if starting.is_some() {
        return Err(acp::Error::invalid_params().data(
            "language server is still starting; retry once _zed.dev/lsp/servers reports it running",
        ));
    }

    Err(acp::Error::invalid_params().data(format!(
        "no language server with id {server_id:?}; call _zed.dev/lsp/servers for the current set"
    )))
}

fn server_workspace_path(
    project: &Project,
    worktree_id: Option<WorktreeId>,
    cx: &App,
) -> Option<PathBuf> {
    Some(
        project
            .worktree_for_id(worktree_id?, cx)?
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
