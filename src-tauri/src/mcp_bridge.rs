//! The app side of the MCP pipe (doc 09 §3, ADR-037) — **the gate lives here**.
//!
//! Claude Desktop spawns the thin `aperture-mcp` stdio binary (reasoning-gateway
//! crate); every `tools/call` it receives is forwarded to this named-pipe
//! server as one JSON line and answered with one JSON line carrying a finished
//! MCP tool result. The four tools all preserve the transparency invariant:
//!
//! - `aperture_get_context(payload_id)` — releases a payload ONLY if the user
//!   approved it in the preview panel (content-bound hash, doc 15 §2b), audits
//!   the release as `cloud_send`, and consumes the session (one-shot).
//! - `aperture_list_recent` — metadata only (ids/intents/approval state).
//! - `aperture_search_history(query)` — ADR-037's locked constraints: the LIKE
//!   retrieval runs over `redaction_flags = 0` rows only (exclusions can never
//!   leak), results are REDACTED, then **staged as a preview session the user
//!   sees on screen**; the tool returns a staging notice, never content. The
//!   gating shape is per-query approval — the payload releases via
//!   `aperture_get_context` after the user's explicit "Approve for Claude".
//! - `aperture_submit_suggestions(json)` — the return channel: schema-checked
//!   and connector-validated (the cloud suggests, only connectors act).
//!
//! The pipe is loopback-local IPC (no socket); the EGRESS surface is the
//! `aperture-mcp` binary's stdout, which is why that binary lives in the
//! gateway crate (two-emitter rule, doc 13 §2).

use std::sync::Arc;

use tauri::Manager;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use aperture_privacy::audit_log::{AuditLog, AuditSink, CloudSendRecord};
use aperture_reasoning_gateway::transports::mcp::{
    MCP_PIPE_NAME, TOOL_GET_CONTEXT, TOOL_LIST_RECENT, TOOL_SEARCH_HISTORY,
    TOOL_SUBMIT_SUGGESTIONS,
};

use crate::app_state::AppState;
use crate::{commands, events};

/// Cap on staged search hits: enough to be useful, small enough to review.
const SEARCH_LIMIT: usize = 20;

/// Create one pipe listener instance with an owner-only DACL (2026-08-15
/// review): `D:P(A;;GA;;;OW)` grants GENERIC_ALL to the object owner (the
/// account running Aperture) and NOTHING to anyone else — another local
/// account can neither connect to the gate nor pull approved payloads. Falls
/// back to the default descriptor (with a loud log) if SDDL conversion fails.
fn create_pipe_instance(first: bool) -> std::io::Result<NamedPipeServer> {
    use windows::core::PCWSTR;
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    // Built once, kept for the process lifetime (each accepted connection
    // re-creates a listener instance — freeing/rebuilding per instance buys
    // nothing and risks a use-after-free).
    static DESCRIPTOR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let sd = *DESCRIPTOR.get_or_init(|| {
        let sddl: Vec<u16> = "D:P(A;;GA;;;OW)".encode_utf16().chain([0]).collect();
        let mut psd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            match ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut psd,
                None,
            ) {
                Ok(()) => psd.0 as usize,
                Err(e) => {
                    tracing::warn!(%e, "MCP pipe DACL build failed — pipe uses the DEFAULT descriptor");
                    0
                }
            }
        }
    });

    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first);
    if sd == 0 {
        return opts.create(MCP_PIPE_NAME);
    }
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd as *mut core::ffi::c_void,
        bInheritHandle: false.into(),
    };
    // SAFETY: `attrs` outlives the call; the descriptor lives forever (above).
    unsafe {
        opts.create_with_security_attributes_raw(
            MCP_PIPE_NAME,
            &mut attrs as *mut SECURITY_ATTRIBUTES as *mut core::ffi::c_void,
        )
    }
}

/// Spawn the pipe server. Non-fatal on failure — MCP is one transport of three.
pub fn spawn(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut server = match create_pipe_instance(true) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(%e, "MCP pipe unavailable — Claude Desktop tools will report Aperture not running");
                return;
            }
        };
        loop {
            if let Err(e) = server.connect().await {
                tracing::warn!(%e, "MCP pipe accept failed");
                continue;
            }
            // Hand the connected instance off and stand up the next listener.
            let connected = server;
            server = match create_pipe_instance(false) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(%e, "MCP pipe re-create failed — bridge stopping");
                    let _ = handle_connection(connected, app.clone()).await;
                    return;
                }
            };
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = handle_connection(connected, app).await;
            });
        }
    });
}

/// Serve one bridge connection: JSON lines in, JSON lines out.
async fn handle_connection(pipe: NamedPipeServer, app: tauri::AppHandle) -> std::io::Result<()> {
    let mut reader = BufReader::new(pipe);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(()); // client closed
        }
        let reply = match serde_json::from_str::<serde_json::Value>(line.trim()) {
            Ok(req) => dispatch(&app, req).await,
            Err(e) => serde_json::json!({ "ok": false, "error": format!("malformed request: {e}") }),
        };
        let mut out = reply.to_string();
        out.push('\n');
        reader.get_mut().write_all(out.as_bytes()).await?;
    }
}

/// Route one tool call. The result is a finished MCP tool result object.
async fn dispatch(app: &tauri::AppHandle, req: serde_json::Value) -> serde_json::Value {
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = req
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let state = app.state::<AppState>();
    let outcome = match name {
        TOOL_GET_CONTEXT => get_context(&state, &args).await,
        TOOL_LIST_RECENT => list_recent(&state).await,
        TOOL_SEARCH_HISTORY => search_history(app, &state, &args).await,
        TOOL_SUBMIT_SUGGESTIONS => submit_suggestions(app, &state, &args).await,
        other => Err(format!("unknown tool: {other}")),
    };
    match outcome {
        Ok(result) => serde_json::json!({ "ok": true, "result": result }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

/// Build the MCP `{content:[{type:"text",…}]}` result shape.
fn text_result(text: impl Into<String>, is_error: bool) -> serde_json::Value {
    serde_json::json!({
        "content": [ { "type": "text", "text": text.into() } ],
        "isError": is_error
    })
}

/// `aperture_get_context` — release an APPROVED payload, audited + consumed.
async fn get_context(
    state: &tauri::State<'_, AppState>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = args
        .get("payload_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let Some(id) = id else {
        return Ok(text_result("payload_id must be a UUID string.", true));
    };

    let mut previews = state.previews.lock().await;
    if !previews.approved.contains_key(&id) {
        return Ok(if previews.sessions.contains_key(&id) {
            text_result(
                format!(
                    "Payload {id} is staged but NOT yet approved. Ask the user to press \
                     \"Approve for Claude\" in the Aperture preview on their screen, then \
                     call this tool again."
                ),
                false,
            )
        } else {
            text_result(format!("Unknown or expired payload_id {id}."), true)
        });
    }
    // Approved: verify content-bound approval, audit, consume, release.
    let approved_hash = previews.approved.remove(&id).expect("checked above");
    let session = previews
        .sessions
        .remove(&id)
        .ok_or_else(|| format!("approved payload {id} has no session"))?;
    // Transport binding (2026-08-15 review): the preview showed the user WHICH
    // transport the approval was for. MCP may release only payloads approved
    // for Claude Desktop — an approval given for a push Send (CLI/API) must
    // stay reserved for that send, so restore it untouched.
    if session.payload().transport_target != aperture_contracts::TransportTarget::ClaudeDesktopMcp {
        previews.sessions.insert(id, session);
        previews.approved.insert(id, approved_hash);
        return Ok(text_result(
            format!("Payload {id} is not available over MCP — the user approved it for a different transport."),
            true,
        ));
    }
    let wire = serde_json::to_vec(session.payload()).map_err(|e| e.to_string())?;
    if aperture_privacy::audit_log::sha256_hex(&wire) != approved_hash {
        // Restore the session (approval stays consumed) so "re-approve" is an
        // instruction the user can actually follow (2026-08-15 review).
        previews.sessions.insert(id, session);
        return Err(format!(
            "payload {id} changed after approval — it must be re-approved (doc 13 §3)"
        ));
    }
    // If the release cannot be recorded, it must not happen (doc 13 §3):
    // restore the session so the user's approval isn't silently consumed.
    let audit = AuditLog::new(Arc::clone(&state.db));
    if let Err(e) = audit.record_cloud_send(CloudSendRecord {
        payload_id: id,
        wire_sha256: approved_hash.clone(),
        transport: aperture_contracts::TransportTarget::ClaudeDesktopMcp,
        byte_count: wire.len() as u64,
        ts: crate::pipeline::epoch_ms(),
    }) {
        previews.sessions.insert(id, session);
        previews.approved.insert(id, approved_hash);
        return Err(format!("cloud_send audit write failed — payload NOT released: {e}"));
    }
    tracing::info!(payload_id = %id, bytes = wire.len(), "approved payload released to Claude Desktop (MCP)");
    Ok(text_result(
        String::from_utf8(wire).map_err(|e| e.to_string())?,
        false,
    ))
}

/// `aperture_list_recent` — staged payload METADATA only, and only for
/// sessions bound to the MCP transport: advertising push-flow (CLI/API)
/// previews here offered Claude payloads the user never approved for MCP
/// (2026-08-15 review).
async fn list_recent(state: &tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let previews = state.previews.lock().await;
    let rows: Vec<serde_json::Value> = previews
        .sessions
        .iter()
        .filter(|(_, s)| {
            s.payload().transport_target == aperture_contracts::TransportTarget::ClaudeDesktopMcp
        })
        .map(|(id, s)| {
            serde_json::json!({
                "payload_id": id.to_string(),
                "intent": format!("{:?}", s.payload().intent),
                "created_ts": s.payload().created_ts,
                "approved": previews.approved.contains_key(id),
            })
        })
        .collect();
    Ok(text_result(
        serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into()),
        false,
    ))
}

/// `aperture_search_history` — ADR-037. Runs the retrieval, redacts, STAGES the
/// results as a preview the user must approve; returns only a staging notice.
///
/// Hardened 2026-08-15 (review, HIGH — the tool was a match/no-match +
/// hit-count oracle over unredacted text):
/// - the reply is CONSTANT: hit and miss stage identically, no count, so the
///   tool-result channel carries zero bits about local history pre-approval;
/// - LIKE wildcards in the query are escaped (no `%`-prefix probing);
/// - a row only counts as a hit if the query still matches AFTER the user's
///   redaction terms are applied — a secret the redactor would mask can never
///   be probed via the match itself;
/// - every search writes an `mcp_search` audit row BEFORE returning; if the
///   audit write fails the search fails.
async fn search_history(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    use aperture_contracts::PayloadItem;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if query.is_empty() {
        return Ok(text_result("query must be a non-empty string.", true));
    }

    // LIKE retrieval over titles/apps/screen text — `redaction_flags = 0` only:
    // excluded/private-window rows can never enter a payload (doc 13 §4).
    // The query is a LITERAL: escape `\`, `%`, `_` and declare ESCAPE.
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let needle = format!("%{escaped}%");
    let hits = state
        .db
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT e.id, e.ts, e.app, e.window_title, substr(sc.ocr_text, 1, 300) \
                 FROM events e LEFT JOIN screen_context sc ON sc.event_id = e.id \
                 WHERE e.redaction_flags = 0 \
                   AND e.type NOT IN ('capture_toggle','cloud_send','mcp_search') \
                   AND (e.window_title LIKE ?1 ESCAPE '\\' OR e.app LIKE ?1 ESCAPE '\\' \
                        OR sc.ocr_text LIKE ?1 ESCAPE '\\') \
                 ORDER BY e.ts DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![needle, SEARCH_LIMIT as i64],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string())?;

    let user_terms = commands::read_user_redaction_terms(&state.db);
    let redactor = aperture_privacy::redaction::Redactor::new(&user_terms)
        .map_err(|e| format!("redaction rules failed to compile: {e}"))?;

    // Match-over-redacted-text: keep a row only if the query is still present
    // once redaction has run on it. This closes the probe channel for exactly
    // the content the user asked to have masked.
    let query_lower = query.to_lowercase();
    let items: Vec<PayloadItem> = hits
        .into_iter()
        .filter_map(|(event_id, ts, hit_app, title, ocr)| {
            let text = format!(
                "[{ts}] {} — {}\n{}",
                hit_app.unwrap_or_default(),
                title.unwrap_or_default(),
                ocr.unwrap_or_default()
            );
            let (redacted_text, _) = redactor.redact_text(&text);
            redacted_text
                .to_lowercase()
                .contains(&query_lower)
                .then_some(PayloadItem::OcrText {
                    source_event_id: event_id,
                    text, // the builder's redactor masks the staged copy below
                    redacted: false,
                })
        })
        .collect();
    let hit_count = items.len();

    // Redact + assemble through the same builder every preview uses (doc 13 §5),
    // stamped for the MCP transport so the panel offers "Approve for Claude".
    // Zero hits stage too: the user sees every probe, and the reply stays flat.
    let (payload, _report) = aperture_reasoning_gateway::payload_builder::build(
        aperture_contracts::Intent::Custom,
        items,
        aperture_contracts::TransportTarget::ClaudeDesktopMcp,
        &redactor,
        crate::pipeline::epoch_ms(),
    )
    .map_err(|e| e.to_string())?;
    let payload_id = payload.payload_id;

    // ADR-037: every search is audited, hit or miss, BEFORE anything returns.
    // The trail — not the tool result — is where hit_count lives.
    let audit = AuditLog::new(Arc::clone(&state.db));
    audit
        .record_mcp_search(aperture_privacy::audit_log::McpSearchRecord {
            query: query.to_string(),
            hit_count: hit_count as u64,
            payload_id: Some(payload_id),
            ts: crate::pipeline::epoch_ms(),
        })
        .map_err(|e| format!("mcp_search audit write failed — search not run: {e}"))?;

    {
        let mut previews = state.previews.lock().await;
        previews.sessions.insert(
            payload_id,
            aperture_reasoning_gateway::preview::PreviewSession::new(payload.clone()),
        );
    }
    // Surface the preview on the user's screen (primary overlay).
    if let Err(e) = events::emit_preview_request(app, &payload) {
        tracing::warn!(%e, "MCP search staged but the preview surface did not open");
    }

    // CONSTANT text — identical for hit and miss (no counts, no query echo
    // beyond what the model already knows it sent).
    Ok(text_result(
        format!(
            "The search was staged for the user's review as payload {payload_id}. \
             Whatever matched (possibly nothing) is on the user's screen — NOTHING \
             has been returned to you, and match results are never disclosed here. \
             If the user presses \"Approve for Claude\", call {TOOL_GET_CONTEXT} \
             with payload_id \"{payload_id}\" to receive what they approved."
        ),
        false,
    ))
}

/// `aperture_submit_suggestions` — the return channel (doc 09 §4): parse the
/// structured shape, re-validate per connector, then SURFACE the survivors as
/// bubbles on the user's screen (US3's last leg on the primary transport —
/// doc 20's amended acceptance criterion; was validate-and-drop until the
/// 2026-08-15 review).
async fn submit_suggestions(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let parsed = aperture_reasoning_gateway::suggestion_validator::parse_response(
        &args.to_string(),
    )
    .map_err(|e| format!("suggestions failed schema validation: {e:?}"))?;
    let total = parsed.suggestions.len();
    let surfaced = crate::pipeline::surface_cloud_suggestions(app, state.inner(), &parsed);
    tracing::info!(total, surfaced, "MCP suggestions received + surfaced (doc 09 §4)");
    Ok(text_result(
        format!(
            "Received {total} suggestion(s); {surfaced} passed connector re-validation \
             and now render as bubbles on the user's screen. The rest were rejected — \
             only Aperture's connectors act, and only on payloads they re-validate."
        ),
        false,
    ))
}
