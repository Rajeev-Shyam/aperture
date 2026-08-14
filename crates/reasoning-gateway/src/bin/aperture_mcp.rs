//! `aperture-mcp` — the stdio MCP server Claude Desktop spawns (doc 09 §3).
//!
//! JSON-RPC 2.0, newline-delimited, over stdin/stdout. This process is a thin
//! bridge: `initialize`/`ping`/`tools/list` are answered locally (the tool
//! surface is static — [`mcp::tool_descriptors`]), and every `tools/call` is
//! forwarded over a local named pipe to the RUNNING Aperture app, where the
//! approval gate lives (src-tauri `mcp_bridge`): only user-approved payloads
//! ever come back, and every return is audit-logged app-side.
//!
//! Two-emitter rule (doc 13 §2): this binary's stdout IS an egress surface —
//! bytes written here land in Claude Desktop. That is why it lives in the
//! reasoning-gateway crate (the sanctioned emitter), and why it never reads
//! the DB or files itself: everything it can ever emit was released by the
//! app across the gate.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use aperture_reasoning_gateway::transports::mcp::{tool_descriptors, MCP_PIPE_NAME};

/// MCP protocol revision answered to `initialize` when the client's own is absent.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// How long a `tools/call` may wait on the app. Generous: the gated tools
/// legitimately wait on a human reading a preview.
const CALL_TIMEOUT: Duration = Duration::from_secs(180);

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    while let Ok(Some(line)) = lines.next_line().await {
        // Tolerate a UTF-8 BOM: some hosts' pipe writers stamp one on the first
        // line, and JSON-RPC dies silently on it.
        let line = line.trim().trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // not JSON: nothing sane to answer (no id to attach)
        };
        let id = msg.get("id").cloned().filter(|v| !v.is_null());
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let response = match (method, &id) {
            ("initialize", Some(_)) => Some(result(&id, initialize_result(&msg))),
            ("ping", Some(_)) => Some(result(&id, serde_json::json!({}))),
            ("tools/list", Some(_)) => {
                Some(result(&id, serde_json::json!({ "tools": tool_descriptors() })))
            }
            ("tools/call", Some(_)) => Some(result(&id, handle_call(&msg).await)),
            // Notifications (initialized/cancelled/…) take no response.
            (_, None) => None,
            (_, Some(_)) => Some(error(&id, -32601, "method not found")),
        };
        if let Some(r) = response {
            let mut out = r.to_string();
            out.push('\n');
            if stdout.write_all(out.as_bytes()).await.is_err() {
                return; // Claude Desktop closed us
            }
            let _ = stdout.flush().await;
        }
    }
}

/// `initialize` result: echo the client's protocolVersion (spec: respond with
/// the requested revision when supported) and advertise the tools capability.
fn initialize_result(msg: &serde_json::Value) -> serde_json::Value {
    let version = msg
        .pointer("/params/protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    serde_json::json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "aperture", "version": env!("CARGO_PKG_VERSION") }
    })
}

/// Bridge one `tools/call` to the running app. Any failure becomes an
/// `isError` text result — never a protocol error, so Claude can relay it.
async fn handle_call(msg: &serde_json::Value) -> serde_json::Value {
    let name = msg
        .pointer("/params/name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let arguments = msg
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    match call_app(name, &arguments).await {
        Ok(result) => result,
        Err(text) => serde_json::json!({
            "content": [ { "type": "text", "text": text } ],
            "isError": true
        }),
    }
}

/// One round-trip over the named pipe: a JSON line out, a JSON line back.
/// The app returns the finished MCP tool result (`{content, isError?}`).
async fn call_app(name: &str, arguments: &serde_json::Value) -> Result<serde_json::Value, String> {
    let client = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(MCP_PIPE_NAME)
        .map_err(|_| {
            "Aperture is not running on this machine — start Aperture, then try again."
                .to_string()
        })?;
    let mut reader = BufReader::new(client);
    let mut request =
        serde_json::json!({ "op": "call", "name": name, "arguments": arguments }).to_string();
    request.push('\n');
    reader
        .get_mut()
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("Aperture bridge write failed: {e}"))?;

    let mut line = String::new();
    tokio::time::timeout(CALL_TIMEOUT, reader.read_line(&mut line))
        .await
        .map_err(|_| "Aperture did not answer in time (is a preview awaiting the user?)".to_string())?
        .map_err(|e| format!("Aperture bridge read failed: {e}"))?;
    let reply: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| format!("Aperture bridge sent malformed JSON: {e}"))?;
    if reply.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| "Aperture bridge reply had no result".to_string())
    } else {
        Err(reply
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("Aperture reported an unknown bridge error")
            .to_string())
    }
}

/// A JSON-RPC 2.0 success envelope.
fn result(id: &Option<serde_json::Value>, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A JSON-RPC 2.0 error envelope.
fn error(id: &Option<serde_json::Value>, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
