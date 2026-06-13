//! Structured-suggestion validation (doc 09 §4).
//!
//! The cloud is *asked* to return [`StructuredSuggestions`] (tool schema on
//! MCP/API, strict JSON instruction on CLI). On receipt we:
//!
//! 1. **Schema-check** the JSON against [`StructuredSuggestions`] — a malformed
//!    body gets one repair round-trip on API/MCP, and on CLI degrades to
//!    rendering `answer_text` / raw prose (doc 09 §6).
//! 2. For every [`CloudSuggestion`], hand `reconstruct_payload` to the **target
//!    connector's** [`Connector::validate`] — *the cloud can suggest, only a
//!    connector can act* (doc 09 §4, doc 15 §3). A suggestion whose payload the
//!    connector rejects (or whose `connector_type` is unknown / `"none"`)
//!    **degrades to `answer_text`** — text-only advice, no action button.
//!
//! Output is the same source-agnostic shape local candidates flatten into, so
//! the Bubble UI treats cloud and local results identically except for the
//! "via Claude" source tag (doc 15 §5).

// TODO(M7:) schema-check + per-connector re-validation land in M7; depends on the
// connector registry (doc 10) being available to look up by `connector_type`.

use aperture_contracts::{Connector, StructuredSuggestions};

/// Errors from validating a cloud response (doc 09 §4/§6).
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    /// The response did not match the [`StructuredSuggestions`] schema and could
    /// not be repaired; the caller falls back to rendering raw prose (doc 09 §6).
    #[error("response is not valid StructuredSuggestions: {0}")]
    SchemaMismatch(String),
}

/// Resolves a `connector_type` string to its [`Connector`] for re-validation
/// (doc 10 §1). Backed by the connector registry; abstracted here so the
/// validator does not depend on the concrete registry type. // TODO(M4:) impl on
/// the `aperture_connectors` registry.
pub trait ConnectorLookup {
    /// Return the connector whose [`Connector::id`] equals `connector_type`, or
    /// `None` for unknown / `"none"` types.
    fn by_type(&self, connector_type: &str) -> Option<&dyn Connector>;
}

/// Schema-check a raw cloud response and re-validate each suggestion against its
/// target connector (doc 09 §4).
///
/// Returns a [`StructuredSuggestions`] whose `suggestions` contains only those
/// the target connector accepted; suggestions that fail connector validation are
/// dropped from the actionable list and their content folds into `answer_text`
/// (degrade-to-text, doc 09 §4). `answer_text` is preserved verbatim.
pub fn validate(
    _raw: StructuredSuggestions,
    _connectors: &dyn ConnectorLookup,
) -> Result<StructuredSuggestions, ValidationError> {
    // TODO(M7:)
    //   for each CloudSuggestion s in raw.suggestions:
    //     match connectors.by_type(&s.connector_type):
    //       Some(c) if c.validate(&s.reconstruct_payload).is_some() => keep (gets a button),
    //       _ => degrade: drop from suggestions, append rationale/title to answer_text.
    //   carry raw.answer_text through unchanged.
    todo!("M7: keep only connector-validated suggestions; degrade the rest to answer_text")
}

/// Parse-and-schema-check a raw JSON body into [`StructuredSuggestions`] (doc 09 §4).
/// Used by transports before [`validate`]; the one repair round-trip on API/MCP
/// (doc 09 §6) is driven by the transport, which re-prompts on `Err` and retries
/// this parse once.
pub fn parse_response(_body: &str) -> Result<StructuredSuggestions, ValidationError> {
    // TODO(M7:) serde_json::from_str::<StructuredSuggestions>(body)
    //           .map_err(|e| ValidationError::SchemaMismatch(e.to_string())).
    todo!("M7: deserialize + schema-check the cloud body into StructuredSuggestions")
}
