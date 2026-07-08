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
/// validator's *callers* can fake it in tests.
pub trait ConnectorLookup {
    /// Return the connector whose [`Connector::id`] equals `connector_type`, or
    /// `None` for unknown / `"none"` types.
    fn by_type(&self, connector_type: &str) -> Option<&dyn Connector>;
}

/// The real registry satisfies the lookup directly (M4). The gateway depending
/// on `aperture-connectors` is the sanctioned direction: the cloud can only
/// *suggest*; connectors — reached through this seam — are the only actors.
impl ConnectorLookup for aperture_connectors::ConnectorRegistry {
    fn by_type(&self, connector_type: &str) -> Option<&dyn Connector> {
        aperture_connectors::ConnectorRegistry::by_type(self, connector_type)
    }
}

/// Schema-check a raw cloud response and re-validate each suggestion against its
/// target connector (doc 09 §4).
///
/// Returns a [`StructuredSuggestions`] whose `suggestions` contains only those
/// the target connector accepted; suggestions that fail connector validation are
/// dropped from the actionable list and their content folds into `answer_text`
/// (degrade-to-text, doc 09 §4). `answer_text` is preserved verbatim.
pub fn validate(
    raw: StructuredSuggestions,
    connectors: &dyn ConnectorLookup,
) -> Result<StructuredSuggestions, ValidationError> {
    let mut kept = Vec::new();
    let mut degraded = String::new();
    for suggestion in raw.suggestions {
        // The cloud can only *suggest*; a connector is the only actor (doc 09 §4,
        // doc 15 §3). Keep it iff its target connector re-validates the payload.
        let accepted = connectors
            .by_type(&suggestion.connector_type)
            .and_then(|c| c.validate(&suggestion.reconstruct_payload))
            .is_some();
        if accepted {
            kept.push(suggestion);
        } else {
            // Degrade to text: fold the title (+ rationale) into answer_text, no button.
            if !degraded.is_empty() {
                degraded.push('\n');
            }
            degraded.push_str(&suggestion.title);
            if !suggestion.rationale.is_empty() {
                degraded.push_str(" — ");
                degraded.push_str(&suggestion.rationale);
            }
        }
    }
    // Preserve any existing answer_text verbatim, appending the degraded lines.
    let answer_text = match (raw.answer_text, degraded.is_empty()) {
        (existing, true) => existing,
        (Some(a), false) if !a.is_empty() => Some(format!("{a}\n{degraded}")),
        (_, false) => Some(degraded),
    };
    Ok(StructuredSuggestions { suggestions: kept, answer_text })
}

/// Parse-and-schema-check a raw JSON body into [`StructuredSuggestions`] (doc 09 §4).
/// Used by transports before [`validate`]; the one repair round-trip on API/MCP
/// (doc 09 §6) is driven by the transport, which re-prompts on `Err` and retries
/// this parse once.
pub fn parse_response(body: &str) -> Result<StructuredSuggestions, ValidationError> {
    serde_json::from_str::<StructuredSuggestions>(body)
        .map_err(|e| ValidationError::SchemaMismatch(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::connector::{ConnectorState, OpenOutcome};
    use aperture_contracts::fakes::FakeConnector;
    use aperture_contracts::suggestions::CloudSuggestion;
    use aperture_contracts::StructuredSuggestions;

    /// A lookup with exactly one connector (id == `ty`) that accepts any payload.
    struct OneConnector {
        ty: &'static str,
        conn: FakeConnector,
    }
    impl ConnectorLookup for OneConnector {
        fn by_type(&self, connector_type: &str) -> Option<&dyn Connector> {
            (connector_type == self.ty).then_some(&self.conn as &dyn Connector)
        }
    }

    fn accepting(ty: &'static str) -> OneConnector {
        OneConnector {
            ty,
            conn: FakeConnector {
                id: ty,
                // `validate` returns `capture_result`; Some(..) ⇒ accept.
                capture_result: Some(ConnectorState {
                    id: "s".into(),
                    connector_type: ty.into(),
                    reconstruct_payload: serde_json::json!({}),
                    payload_version: 1,
                    captured_ts: 0,
                    stale_after_ts: None,
                }),
                open_result: OpenOutcome::Resumed,
            },
        }
    }

    fn sugg(title: &str, ty: &str) -> CloudSuggestion {
        CloudSuggestion {
            title: title.into(),
            connector_type: ty.into(),
            reconstruct_payload: serde_json::json!({ "v": 1 }),
            rationale: "because".into(),
        }
    }

    #[test]
    fn keeps_connector_validated_and_degrades_unknown_to_text() {
        let raw = StructuredSuggestions {
            suggestions: vec![sugg("Resume video", "youtube"), sugg("Do a barrel roll", "spaceship")],
            answer_text: None,
        };
        let out = validate(raw, &accepting("youtube")).unwrap();
        assert_eq!(out.suggestions.len(), 1, "only the youtube suggestion has an actor");
        assert_eq!(out.suggestions[0].connector_type, "youtube");
        let text = out.answer_text.expect("the unknown one degraded to text");
        assert!(text.contains("Do a barrel roll") && text.contains("because"), "got {text}");
    }

    #[test]
    fn none_actionable_folds_all_into_answer_text() {
        let raw = StructuredSuggestions {
            suggestions: vec![sugg("A", "nope")],
            answer_text: Some("prior".into()),
        };
        let out = validate(raw, &accepting("youtube")).unwrap();
        assert!(out.suggestions.is_empty());
        let text = out.answer_text.unwrap();
        assert!(text.starts_with("prior"), "existing answer_text preserved: {text}");
        assert!(text.contains('A'));
    }

    #[test]
    fn parse_response_accepts_valid_and_rejects_garbage() {
        let ok = parse_response(r#"{"suggestions":[],"answer_text":"hi"}"#).unwrap();
        assert_eq!(ok.answer_text.as_deref(), Some("hi"));
        assert!(parse_response("not json").is_err());
    }
}
