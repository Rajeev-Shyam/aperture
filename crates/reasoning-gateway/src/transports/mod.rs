//! The three swappable transports (doc 09 §3) — and the push/pull asymmetry
//! that shapes the UX.
//!
//! All three implement [`aperture_contracts::ReasoningTransport`] and live
//! **only** inside this crate (the two-emitter rule, doc 13 §2): they are the
//! sole code paths that open a socket or spawn the Claude CLI.
//!
//! | Transport | Direction | Gate placement (doc 09 §3) |
//! |---|---|---|
//! | [`cli`] (Claude Code CLI) | **Push** — Aperture initiates | Preview shown *before* spawning; approved bytes form the prompt. |
//! | [`mcp`] (Claude Desktop via MCP) | **Pull** — Claude initiates | Gate lives *inside* the `aperture_get_context` tool handler, which blocks on the user's Send. |
//! | [`api`] (Messages API) | **Push** | Preview *before* the HTTPS call. |
//!
//! Default fall-through order: CLI -> Desktop-MCP -> API, user-reorderable
//! (doc 09 §3). Health failures fall through with a visible notice; offline
//! means the local answer stands and nothing queues silently (doc 09 §6).

// TODO(M7:) all three transports implement ReasoningTransport in M7.

pub mod api;
pub mod cli;
pub mod mcp;
