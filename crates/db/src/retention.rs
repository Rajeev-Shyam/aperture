//! Retention & lifecycle (doc 03 §6, doc 13 §7). A nightly job enforces TTLs.
//! Defaults are user-configurable in `settings`.

/// Default TTLs in days (doc 03 §6). All [ASSUMPTION] in the spec; user-adjustable.
pub struct RetentionPolicy {
    pub events_days: u32,        // 90: events + ctx_vec (vec rows cascade)
    pub ocr_text_days: u32,      // 30: nullify ocr_text, keep event skeleton
    pub voice_days: u32,         // 30: voice_utterance transcript scrub
    pub suggestions_days: u32,   // 180: suggestions + patterns
    pub audit_days: u32,         // 30: capture_toggle + cloud_send survive purge this long
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            events_days: 90,
            ocr_text_days: 30,
            voice_days: 30,
            suggestions_days: 180,
            audit_days: 30,
        }
    }
}

/// Run the nightly pruner. Raw frames are never persisted, so there is nothing
/// to prune there (doc 03 §6, doc 13 §4).
pub fn run_nightly_prune(_now_ms: i64, _policy: &RetentionPolicy) -> Result<(), crate::DbError> {
    todo!("M9: delete by ts; nullify OCR text; cascade ctx_vec; preserve audit rows")
}
