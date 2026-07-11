//! Live in-process protocol smoke for the stdio server lifecycle and features.

use std::error::Error;
use std::thread;

use aion_awl_lsp::run_connection;
use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::json;

type TestResult = Result<(), Box<dyn Error>>;

fn send(connection: &Connection, message: Message) -> TestResult {
    connection.sender.send(message)?;
    Ok(())
}

fn receive(connection: &Connection) -> Result<Message, Box<dyn Error>> {
    Ok(connection.receiver.recv()?)
}

#[test]
fn live_initialize_open_diagnose_format_shutdown_exchange() -> TestResult {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(move || run_connection(&server));

    send(
        &client,
        Message::Request(Request::new(
            RequestId::from(1),
            "initialize".to_owned(),
            json!({ "capabilities": {} }),
        )),
    )?;
    let Message::Response(initialize) = receive(&client)? else {
        return Err("server did not respond to initialize".into());
    };
    assert!(
        initialize.error.is_none(),
        "initialize failed: {initialize:?}"
    );
    let capabilities = initialize
        .result
        .ok_or("initialize response omitted capabilities")?;
    assert_eq!(capabilities["capabilities"]["positionEncoding"], "utf-16");
    assert_eq!(
        capabilities["capabilities"]["textDocumentSync"]["change"],
        1
    );

    send(
        &client,
        Message::Notification(Notification::new("initialized".to_owned(), json!({}))),
    )?;
    let uri = "file:///tmp/broken.awl";
    let broken = "//! Broken route.\nworkflow probe\n  outcome done: type String, route success\n\nstep one\n  route missing\n";
    send(
        &client,
        Message::Notification(Notification::new(
            "textDocument/didOpen".to_owned(),
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "awl",
                    "version": 1,
                    "text": broken
                }
            }),
        )),
    )?;
    let Message::Notification(published) = receive(&client)? else {
        return Err("server did not publish diagnostics".into());
    };
    assert_eq!(published.method, "textDocument/publishDiagnostics");
    let diagnostics = published.params["diagnostics"]
        .as_array()
        .ok_or("diagnostics payload is not an array")?;
    assert!(!diagnostics.is_empty(), "broken document checked clean");
    assert_eq!(diagnostics[0]["source"], "awl");

    send(
        &client,
        Message::Request(Request::new(
            RequestId::from(2),
            "textDocument/formatting".to_owned(),
            json!({
                "textDocument": { "uri": uri },
                "options": { "tabSize": 2, "insertSpaces": true }
            }),
        )),
    )?;
    let Message::Response(formatting) = receive(&client)? else {
        return Err("server did not respond to formatting".into());
    };
    assert!(
        formatting.error.is_none(),
        "formatting failed: {formatting:?}"
    );
    assert!(
        formatting.result.is_some_and(|value| value.is_array()),
        "formatting response was not an edit array"
    );

    send(
        &client,
        Message::Request(Request::new(
            RequestId::from(3),
            "shutdown".to_owned(),
            serde_json::Value::Null,
        )),
    )?;
    let Message::Response(shutdown) = receive(&client)? else {
        return Err("server did not respond to shutdown".into());
    };
    assert!(shutdown.error.is_none(), "shutdown failed: {shutdown:?}");
    send(
        &client,
        Message::Notification(Notification::new(
            "exit".to_owned(),
            serde_json::Value::Null,
        )),
    )?;
    let server_result = server_thread.join().map_err(|_| "server thread panicked")?;
    server_result?;
    Ok(())
}

#[test]
fn rev2_corpus_hover_and_definition_use_checker_semantics() -> TestResult {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aion-awl/tests/fixtures/rev2/header-types/valid/doc_comments.awl");
    let source = std::fs::read_to_string(&fixture)?;
    let uri = format!("file://{}", fixture.display());
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(move || run_connection(&server));

    send(
        &client,
        Message::Request(Request::new(
            RequestId::from(10),
            "initialize".to_owned(),
            json!({ "capabilities": {} }),
        )),
    )?;
    let Message::Response(initialize) = receive(&client)? else {
        return Err("server did not respond to initialize".into());
    };
    let capabilities = initialize.result.ok_or("initialize omitted capabilities")?;
    assert_eq!(capabilities["capabilities"]["hoverProvider"], true);
    assert_eq!(capabilities["capabilities"]["definitionProvider"], true);
    send(
        &client,
        Message::Notification(Notification::new("initialized".to_owned(), json!({}))),
    )?;
    send(
        &client,
        Message::Notification(Notification::new(
            "textDocument/didOpen".to_owned(),
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "awl",
                    "version": 1,
                    "text": source
                }
            }),
        )),
    )?;
    let Message::Notification(published) = receive(&client)? else {
        return Err("server did not publish corpus diagnostics".into());
    };
    assert_eq!(published.method, "textDocument/publishDiagnostics");
    assert_eq!(published.params["diagnostics"], json!([]));

    assert_semantic_requests(&client, &uri)?;

    send(
        &client,
        Message::Request(Request::new(
            RequestId::from(13),
            "shutdown".to_owned(),
            serde_json::Value::Null,
        )),
    )?;
    let Message::Response(shutdown) = receive(&client)? else {
        return Err("server did not respond to shutdown".into());
    };
    assert!(shutdown.error.is_none(), "shutdown failed: {shutdown:?}");
    send(
        &client,
        Message::Notification(Notification::new(
            "exit".to_owned(),
            serde_json::Value::Null,
        )),
    )?;
    let server_result = server_thread.join().map_err(|_| "server thread panicked")?;
    server_result?;
    Ok(())
}

fn assert_semantic_requests(client: &Connection, uri: &str) -> TestResult {
    send(
        client,
        Message::Request(Request::new(
            RequestId::from(11),
            "textDocument/hover".to_owned(),
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 21, "character": 34 }
            }),
        )),
    )?;
    let Message::Response(hover) = receive(client)? else {
        return Err("server did not respond to hover".into());
    };
    assert!(hover.error.is_none(), "hover failed: {hover:?}");
    let hover = hover.result.ok_or("hover returned null")?;
    assert_eq!(hover["contents"]["kind"], "markdown");
    let markdown = hover["contents"]["value"]
        .as_str()
        .ok_or("hover markdown is not a string")?;
    assert!(markdown.contains("type Document: Document"), "{markdown}");
    assert!(
        markdown.contains("A fetched document, exactly as retrieved."),
        "{markdown}"
    );

    send(
        client,
        Message::Request(Request::new(
            RequestId::from(12),
            "textDocument/definition".to_owned(),
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 25, "character": 10 }
            }),
        )),
    )?;
    let Message::Response(definition) = receive(client)? else {
        return Err("server did not respond to definition".into());
    };
    assert!(
        definition.error.is_none(),
        "definition failed: {definition:?}"
    );
    let definition = definition.result.ok_or("definition returned null")?;
    assert_eq!(definition["uri"], uri);
    assert_eq!(definition["range"]["start"]["line"], 21);
    assert_eq!(definition["range"]["start"]["character"], 9);
    Ok(())
}
