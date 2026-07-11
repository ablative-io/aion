use std::collections::HashMap;
use std::path::PathBuf;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentFormattingParams, GotoDefinitionParams, OneOf, PositionEncodingKind,
    PublishDiagnosticsParams, ServerCapabilities, TextDocumentPositionParams,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, TextEdit, Uri,
};
use serde::de::DeserializeOwned;

use crate::analysis::{diagnostics, format_document, position_at};
use crate::navigation;

/// A fatal transport or protocol error from the AWL language server.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The peer violated the LSP lifecycle protocol.
    #[error("LSP protocol error: {0}")]
    Protocol(#[from] lsp_server::ProtocolError),
    /// Server capabilities could not be encoded.
    #[error("failed to encode LSP capabilities: {0}")]
    Json(#[from] serde_json::Error),
    /// The stdio transport threads could not finish cleanly.
    #[error("LSP stdio transport error: {0}")]
    Io(#[from] std::io::Error),
    /// The peer closed its channel while the server was sending a message.
    #[error("LSP peer disconnected")]
    Disconnected,
    /// A full-sync notification did not contain a replacement document.
    #[error("textDocument/didChange contained no full document text")]
    MissingFullDocument,
    /// A server-only connection unexpectedly received a response.
    #[error("language server received an unexpected client response")]
    UnexpectedResponse,
}

/// Runs the AWL language server over stdin and stdout until shutdown and exit.
///
/// Protocol traffic owns stdout; callers should report a returned fatal error
/// on stderr only.
///
/// # Errors
///
/// Returns an error when initialization, the protocol channel, or stdio
/// transport fails.
pub fn run_stdio() -> Result<(), ServerError> {
    let (connection, io_threads) = Connection::stdio();
    run_connection(&connection)?;
    io_threads.join()?;
    Ok(())
}

/// Runs a fully initialized AWL server on an existing LSP connection.
///
/// This is public so protocol tests and embedders can use `Connection::memory`
/// while exercising the same initialization and request loop as stdio.
///
/// # Errors
///
/// Returns an error for a fatal lifecycle, serialization, or channel failure.
pub fn run_connection(connection: &Connection) -> Result<(), ServerError> {
    let capabilities = serde_json::to_value(capabilities())?;
    drop(connection.initialize(capabilities)?);
    let mut documents = HashMap::new();
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                handle_request(connection, request, &documents)?;
            }
            Message::Notification(notification) => {
                handle_notification(connection, notification, &mut documents)?;
            }
            Message::Response(_) => return Err(ServerError::UnexpectedResponse),
        }
    }
    Ok(())
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                ..TextDocumentSyncOptions::default()
            },
        )),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        definition_provider: Some(OneOf::Left(true)),
        ..ServerCapabilities::default()
    }
}

fn handle_notification(
    connection: &Connection,
    notification: Notification,
    documents: &mut HashMap<String, String>,
) -> Result<(), ServerError> {
    match notification.method.as_str() {
        "textDocument/didOpen" => {
            let params: DidOpenTextDocumentParams = decode_notification(notification)?;
            let uri = params.text_document.uri;
            let version = params.text_document.version;
            documents.insert(uri.as_str().to_owned(), params.text_document.text);
            publish(connection, &uri, version, documents)
        }
        "textDocument/didChange" => {
            let params: DidChangeTextDocumentParams = decode_notification(notification)?;
            let uri = params.text_document.uri;
            let version = params.text_document.version;
            let Some(change) = params.content_changes.into_iter().last() else {
                return Err(ServerError::MissingFullDocument);
            };
            documents.insert(uri.as_str().to_owned(), change.text);
            publish(connection, &uri, version, documents)
        }
        "textDocument/didClose" => {
            let params: DidCloseTextDocumentParams = decode_notification(notification)?;
            let uri = params.text_document.uri;
            documents.remove(uri.as_str());
            let params = PublishDiagnosticsParams::new(uri, Vec::new(), None);
            send(
                connection,
                Message::Notification(lsp_server::Notification::new(
                    "textDocument/publishDiagnostics".to_owned(),
                    params,
                )),
            )
        }
        _ => Ok(()),
    }
}

fn decode_notification<T: DeserializeOwned>(notification: Notification) -> Result<T, ServerError> {
    serde_json::from_value(notification.params).map_err(ServerError::from)
}

fn publish(
    connection: &Connection,
    uri: &Uri,
    version: i32,
    documents: &HashMap<String, String>,
) -> Result<(), ServerError> {
    let Some(source) = documents.get(uri.as_str()) else {
        return Err(ServerError::Disconnected);
    };
    let root = document_root(uri);
    let diagnostics = diagnostics(source, root.as_deref());
    let params = PublishDiagnosticsParams::new(uri.clone(), diagnostics, Some(version));
    send(
        connection,
        Message::Notification(lsp_server::Notification::new(
            "textDocument/publishDiagnostics".to_owned(),
            params,
        )),
    )
}

fn document_root(uri: &Uri) -> Option<PathBuf> {
    let url = url::Url::parse(uri.as_str()).ok()?;
    let path = url.to_file_path().ok()?;
    path.parent().map(PathBuf::from)
}

fn handle_request(
    connection: &Connection,
    request: Request,
    documents: &HashMap<String, String>,
) -> Result<(), ServerError> {
    match request.method.as_str() {
        "textDocument/formatting" => {
            let Some(params) = decode_request::<DocumentFormattingParams>(connection, &request)?
            else {
                return Ok(());
            };
            let edits = documents
                .get(params.text_document.uri.as_str())
                .map_or_else(Vec::new, |source| formatting_edits(source));
            respond(connection, request.id, edits)
        }
        "textDocument/documentSymbol" => {
            let Some(params) =
                decode_request::<lsp_types::DocumentSymbolParams>(connection, &request)?
            else {
                return Ok(());
            };
            let symbols = documents
                .get(params.text_document.uri.as_str())
                .and_then(|source| navigation::document_symbols(source))
                .unwrap_or_else(|| serde_json::json!([]));
            respond(connection, request.id, symbols)
        }
        "textDocument/definition" => {
            let Some(params) = decode_request::<GotoDefinitionParams>(connection, &request)? else {
                return Ok(());
            };
            let TextDocumentPositionParams {
                text_document,
                position,
            } = params.text_document_position_params;
            let location = documents
                .get(text_document.uri.as_str())
                .and_then(|source| navigation::definition(source, &text_document.uri, position));
            respond(connection, request.id, location)
        }
        _ => {
            let response = Response::new_err(
                request.id,
                ErrorCode::MethodNotFound as i32,
                format!("unsupported request: {}", request.method),
            );
            send(connection, Message::Response(response))
        }
    }
}

fn decode_request<T: DeserializeOwned>(
    connection: &Connection,
    request: &Request,
) -> Result<Option<T>, ServerError> {
    match serde_json::from_value(request.params.clone()) {
        Ok(params) => Ok(Some(params)),
        Err(error) => {
            let response = Response::new_err(
                request.id.clone(),
                ErrorCode::InvalidParams as i32,
                error.to_string(),
            );
            send(connection, Message::Response(response))?;
            Ok(None)
        }
    }
}

fn formatting_edits(source: &str) -> Vec<TextEdit> {
    let Some(formatted) = format_document(source) else {
        return Vec::new();
    };
    if formatted == source {
        return Vec::new();
    }
    vec![TextEdit::new(
        lsp_types::Range::new(
            lsp_types::Position::new(0, 0),
            position_at(source, source.len()),
        ),
        formatted,
    )]
}

fn respond<T: serde::Serialize>(
    connection: &Connection,
    id: lsp_server::RequestId,
    value: T,
) -> Result<(), ServerError> {
    send(connection, Message::Response(Response::new_ok(id, value)))
}

fn send(connection: &Connection, message: Message) -> Result<(), ServerError> {
    connection
        .sender
        .send(message)
        .map_err(|_| ServerError::Disconnected)
}
