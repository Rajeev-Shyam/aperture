//! Typed IPC boundary between the WebView overlay and the Rust core (doc 11 §1,
//  doc 15). Every cross-process call goes through here so the rest of the UI
//  never touches the raw `invoke`/`listen` strings — and so the TS mirror of the
//  Rust contracts lives in exactly one place (drift = a type error here, not a
//  runtime surprise).
//
//  TRANSPARENCY INVARIANT (doc 13 §2, the two-emitter rule): the UI opens NO
//  network sockets and spawns NO processes. Every command below is a message to
//  the Rust core; only the reasoning-gateway crate, behind `preview_send`, may
//  emit bytes to the network. The CSP in index.html enforces this at the browser
//  level too.
//
//  These TS types MIRROR `aperture_contracts` (Rust). They are intentionally
//  structural copies — keep them additive-only and tolerant of unknown fields,
//  matching the Rust compatibility law (doc 15 §6).

import { invoke } from "@tauri-apps/api/core"; // [VERIFY] tauri v2 module path
import { listen, type UnlistenFn, type Event as TauriEvent } from "@tauri-apps/api/event"; // [VERIFY]

// ---------------------------------------------------------------------------
// Contract mirrors (Rust: crates/contracts/src/*.rs)
// ---------------------------------------------------------------------------

/** Mirror of `suggestions::SuggestionSource`. The ONLY thing the UI treats
 *  differently is the small "via Claude" source tag. */
export type SuggestionSource = "local" | "claude";

/** Mirror of `suggestions::ExclusionOffer` — a ready-to-apply "stop capturing
 *  this" rule for the bubble's ⋯ menu (decision #8). The core derives BOTH the
 *  match kind and the (escaped) pattern, because a hand-built regex that fails
 *  to compile becomes a rule the Privacy panel shows as active protection while
 *  it silently matches nothing. The UI only renders `label` and passes the pair
 *  straight to `addExclusion`. */
export interface ExclusionOffer {
  /** What the menu item names, e.g. `"docs.rs"` or `"Slack"`. */
  label: string;
  match_kind: ExclusionKind;
  pattern: string;
}

/** Mirror of `suggestions::BubbleSpec` (contracts/src/suggestions.rs).
 *  `action_ref` resolves to a connector on click (Critical Path B, doc 02 §5). */
export interface BubbleSpec {
  title: string;
  /** Connector-type glyph — a SEMANTIC token (`"video" | "globe" | "doc" |
   *  "code" | "switch" | "spark"`), not a character. Presentation is the UI's
   *  (see `glyphMark` in Bubble.tsx), so persisted rows and new ones render the
   *  same mark even after the design pass changes it. */
  glyph: string;
  /** e.g. `"12:34 · 2h ago"`. */
  sublabel: string | null;
  action_ref: string;
  source: SuggestionSource;
  confidence: number;
  /** Epoch ms this suggestion was created — the freshness half of the slot
   *  admission score (decision #5). Absent/null on rows written before the
   *  column existed; the score then falls back to confidence alone. */
  created_ts?: number | null;
  /** One-click exclusion rules for the ⋯ menu (decision #8). May be empty:
   *  nothing honestly derivable (e.g. an IDE payload names no process). */
  exclusion_offers?: ExclusionOffer[];
}

/** Mirror of `connector::OpenOutcome` (externally-tagged serde). The result of a
 *  bubble click / resume (Critical Path B, doc 02 §5, doc 10 §6) — lets the UI
 *  honest-degrade on `Degraded`/`Failed`. */
export type OpenOutcome =
  | "Resumed"
  | { Degraded: { reason: string } }
  | { Failed: { reason: string } };

/** Mirror of `context_payload::Intent`. */
export type Intent =
  | "summarize_current"
  | "answer_query"
  | "explain_pattern"
  | "custom";

/** Mirror of `context_payload::TransportTarget` (kebab-case on the wire). */
export type TransportTarget = "claude-cli" | "claude-desktop-mcp" | "messages-api";

/** Mirror of `context_payload::PayloadItem` (internally tagged by `kind`,
 *  snake_case). What you see in the preview is exactly what ships. */
export type PayloadItem =
  | { kind: "ocr_text"; source_event_id: number; text: string; redacted?: boolean }
  | { kind: "event_trail"; events: unknown[] }
  | { kind: "connector"; type: string; payload: unknown }
  | { kind: "screenshot"; width: number; height: number; data_b64: string }
  | { kind: "user_addition"; text: string };

/** Mirror of `context_payload::Redaction` — one applied rule + its hit count. */
export interface Redaction {
  /** e.g. `"email"`, `"window_excluded: 1Password"`, `"secret_key"`. */
  rule: string;
  count: number;
}

/** Mirror of `context_payload::ContextPayload` (`aperture/context-payload/v1`).
 *  Note `user_approved` is `#[serde(skip_serializing)]` on the Rust side: it is
 *  the in-process approval flag and is NOT part of the wire schema. The panel
 *  may carry it locally, but Send transmits the serialized object WITHOUT it. */
export interface ContextPayload {
  payload_id: string; // uuid
  created_ts: number;
  intent: Intent;
  items: PayloadItem[];
  redactions: Redaction[];
  enrichment_offered?: boolean;
  transport_target: TransportTarget;
}

/** Mirror of `context_payload::PAYLOAD_SIZE_WARN_BYTES` (doc 09 §5): hard warn
 *  threshold for the serialized payload size. */
export const PAYLOAD_SIZE_WARN_BYTES = 50 * 1024;

/** Mirror of `context_payload::EVENT_TRAIL_MAX` (doc 03 §4): `event_trail` cap. */
export const EVENT_TRAIL_MAX = 50;

/** Mirror of `suggestions::CloudSuggestion`. */
export interface CloudSuggestion {
  title: string;
  /** `"browser" | "youtube" | "document" | "ide" | "none"`. */
  connector_type: string;
  reconstruct_payload?: unknown;
  rationale: string;
}

/** Mirror of `suggestions::StructuredSuggestions` — the source-agnostic shape
 *  local candidates and cloud results both flatten into (doc 09 §4). */
export interface StructuredSuggestions {
  suggestions?: CloudSuggestion[];
  answer_text?: string | null;
}

/** Mirror of `reasoning::TransportId` / `reasoning::Health`. Surfaced in the
 *  preview footer as the transport target + health dot. */
export type TransportId = TransportTarget;
export type Health =
  | { kind: "ready" }
  | { kind: "needs_setup"; detail: string }
  | { kind: "unavailable"; detail: string };

// ---------------------------------------------------------------------------
// Settings (mirror of config/settings.default.json — the seed; at runtime these
// live inside the encrypted DB, doc 13 §6). Typed loosely + additive so the UI
// tolerates server-side additions (doc 15 §6).
// ---------------------------------------------------------------------------

export interface UiSettings {
  max_concurrent_bubbles: number;
  /** How long a bubble sits in `idle` before mild-decay expiry (decision #7).
   *  Read live: a `settings_changed` event re-applies it to bubbles that have
   *  not started their dwell yet — no restart. */
  bubble_dwell_sec: number;
  max_glass_surfaces: number;
  /** Half-life, seconds, of the freshness factor in the slot-admission score
   *  (decision #5). Advanced/settings-only: it tunes queue ordering, not
   *  anything the user can see happen. */
  bubble_freshness_half_life_sec?: number;
  /** Where the HUD cluster (indicator + buttons) is anchored — user-draggable. */
  hud_anchor?: HudAnchor;
}

/** The 8 snap positions for the HUD cluster (4 corners + 4 edge midpoints). */
export type HudAnchor =
  | "top-left"
  | "top-center"
  | "top-right"
  | "center-left"
  | "center-right"
  | "bottom-left"
  | "bottom-center"
  | "bottom-right";

/** The `voice` settings section (mirror of the seed's `voice` block). The
 *  Dashboard Voice tab edits `intent_confidence_floor` (decision #27); writes
 *  must merge the WHOLE section — `set_settings` replaces top-level keys. */
export interface VoiceSettings {
  ptt_hotkey?: string;
  vad_speech_floor_ms?: number;
  max_utterance_sec?: number;
  /** Confirm-before-acting floor, clamped to [0,1] core-side; 1 = always
   *  confirm (decision #27). */
  intent_confidence_floor?: number;
  [key: string]: unknown;
}

/** The `reasoning` settings section. The Dashboard's Advanced tab edits
 *  `transport_order` (decision #39); like `voice`, writes must merge the WHOLE
 *  section — `set_settings` replaces top-level keys. Changing it REBUILDS the
 *  gateway core-side, so the new order is live on the next Send. */
export interface ReasoningSettings {
  transport_order?: TransportTarget[];
  payload_size_warn_kb?: number;
  per_send_approval?: boolean;
  [key: string]: unknown;
}

/** The `pattern_engine` settings section (decision #17). Every key is optional
 *  and range-validated core-side — an out-of-range value falls back to the
 *  crate constant rather than weakening the engine. */
export interface PatternEngineSettings {
  tau_conf?: number;
  cold_start_support_floor?: number;
  semantic_similarity_threshold?: number;
  cooldown_min?: number;
  cap_per_hour_floor?: number;
  cap_per_hour_ceiling?: number;
  cap_per_hour_default?: number;
  session_gap_cold_start_min?: number;
  half_life_sequence_days?: number;
  half_life_temporal_days?: number;
  [key: string]: unknown;
}

export interface Settings {
  ui?: UiSettings;
  reasoning?: ReasoningSettings;
  pattern_engine?: PatternEngineSettings;
  voice?: VoiceSettings;
  // …other sections (capture/loadout/pattern_engine/privacy) are passed
  // through untyped; the overlay reads the `ui` + `reasoning` + `voice` blocks.
  [section: string]: unknown;
}

// ---------------------------------------------------------------------------
// Event payloads (the five events the overlay subscribes to). These shapes are
// the UI<->core contract for `event::emit`; keep additive.
// ---------------------------------------------------------------------------

/** `"bubble_spec"` — the pattern/suggestion pipeline pushes a bubble to render
 *  (doc 08 §6 -> doc 11 §3). Carries an id so lifecycle events can refer back. */
export interface BubbleSpecEvent {
  /** Stable id for this bubble instance (used in `suggestion_lifecycle`). */
  id: string;
  spec: BubbleSpec;
}

/** `"gpu_busy"` — mutex-derived observable (doc 12 §3, doc 14 §5). `true` =>
 *  swap glass to the opaque fallback, fades only, no new blur. */
export type GpuBusyEvent = boolean;

/** `"capture_indicator"` — capture on/off + a transient pulse on activity
 *  (doc 11, doc 12 §6). OFF must visibly reflect VRAM->~0 within 3 s. */
export interface CaptureIndicatorEvent {
  capturing: boolean;
  /** Optional one-line status, e.g. `"releasing… sidecars down"`. */
  detail?: string | null;
}

/** `"voice_surface"` — drives the listening pill / transcript chip / answer
 *  bubble (doc 07, doc 11 §5). A tagged union keyed by `surface`. */
export type VoiceSurfaceEvent =
  | {
      surface: "listening";
      level?: number;
      /** Elapsed hold time (ms), streamed from the core's own mic loop at
       *  ~10 Hz — the same clock that enforces the force-finalize (decision
       *  #28), so the pill's countdown can never drift from the cutoff. */
      elapsed_ms?: number;
      /** The force-finalize ceiling (ms) — `MAX_UTTERANCE`, 30 000. */
      max_ms?: number;
    }
  | { surface: "thinking" } // model swap latency (doc 07): show "thinking", don't fail
  | {
      surface: "transcript";
      text: string;
      confidence: number; // < 0.6 => "Did you say …?" chip (doc 07 §4)
    }
  | {
      surface: "answer";
      title: string;
      /** e.g. `"from your history, yesterday 14:02"`. */
      source: string | null;
      /** present iff the hit is resumable. */
      action_ref?: string | null;
      /** true once "Ask Claude" is allowed (gated). */
      can_ask_claude?: boolean;
    }
  | { surface: "empty"; message: string } // honest empty state (doc 07)
  | { surface: "hidden" };

/** `"suggestion_lifecycle"` — the core may also broadcast lifecycle transitions
 *  (e.g. expiry driven server-side). Mirrors the states in doc 11 §3. */
export type BubbleLifecycleState =
  | "queued"
  | "entering"
  | "idle"
  | "clicked"
  | "dismissed"
  | "expired"
  | "exit";

export interface SuggestionLifecycleEvent {
  id: string;
  state: BubbleLifecycleState;
}

/** `"suggestion_rated"` — a 👍/👎 was recorded (decision #10). Broadcast so the
 *  pressed thumb lights on EVERY monitor's copy of that bubble; the set thumb is
 *  the only receipt the user gets that the signal landed, and it was appearing
 *  on the window the click hit and nowhere else. Emitted only when the rating
 *  actually changed, so it can never re-announce a no-op. */
export interface SuggestionRatedEvent {
  id: string;
  rating: "up" | "down";
}

/** `"settings_changed"` — a `set_settings` write landed (decisions #7/#17/#39).
 *  Surfaces that cached a settings value re-read it instead of waiting for a
 *  restart. `sections` names the patch's top-level keys, so a listener can
 *  ignore writes that are not its own. */
export interface SettingsChangedEvent {
  sections: string[];
}

/** `"audit_alert"` — an audit-trail write failed (decision #41): what egressed
 *  and what the `cloud_send` trail records now disagree. The primary overlay
 *  renders a dismissible warning banner; it never auto-hides. */
export interface AuditAlertEvent {
  message: string;
}

/** `"suggestions_refresh"` — the snooze lifted (ADR-040/Q95; SDLC review
 *  2026-08-19 finding 2): rows queued while it was on are now surfaceable, so
 *  the container re-runs `list_suggestions`. Emitted by `set_snooze` when the
 *  user turns it off and by the core's one-shot timer at a timed deadline. */
export interface SuggestionsRefreshEvent {
  reason: "snooze_off" | "snooze_expired";
}

/** `"dashboard_open"` / `"privacy_open"` / `"preview_claimed"` — a control
 *  surface belongs to the window labeled `target` (decision #13). Broadcast:
 *  the target window opens/keeps the surface, every other window closes its
 *  copy — controls follow the cursor and exactly one instance exists. */
export interface ControlOpenEvent {
  target: string;
}

/** `"vlm_fetch"` — progress/terminal state of the user-initiated VLM weight
 *  download (decision #30). Byte counts are OVERALL across both weight files,
 *  so one progress bar renders directly. `done` means every file verified at
 *  its expected size — VLM is live on its next use, no restart. `error` keeps
 *  any resumable partial file, so retrying continues where it stopped. */
export interface VlmFetchEvent {
  phase: "downloading" | "done" | "error";
  /** The artifact currently downloading (label under the bar). */
  file?: string | null;
  received_bytes: number;
  total_bytes: number;
  error?: string | null;
}

// ---------------------------------------------------------------------------
// Commands (invoke). Names MUST match the src-tauri agent's `#[tauri::command]`
// definitions exactly (doc 11 §1). Bodies are thin wrappers; no logic lives here.
// ---------------------------------------------------------------------------

/** Master capture toggle (doc 12 §6, doc 13 §8). OFF releases capture, kills the
 *  sidecars, and drives VRAM->~0 in <3 s — the third invariant. Returns the new
 *  capturing state. */
export function toggleCapture(on: boolean): Promise<boolean> {
  return invoke<boolean>("toggle_capture", { on });
}

/** Pull the current set of live suggestions (e.g. after a WebView2 respawn, the
 *  queue survives in SQLite — doc 11 §7). */
export function listSuggestions(): Promise<BubbleSpecEvent[]> {
  return invoke<BubbleSpecEvent[]>("list_suggestions");
}

/** Primary bubble action: resolve `action_ref` -> connector -> open (Critical
 *  Path B, doc 02 §5). The core records `suggestion_clicked`. */
export function bubbleClick(id: string, actionRef: string): Promise<OpenOutcome> {
  return invoke<OpenOutcome>("bubble_click", { id, actionRef });
}

/** Bubble feedback into the durable suggestions row + the engine's decay/mute
 *  ladder (doc 08 §7, Q81). Without this, dismissed bubbles resurrect on a
 *  WebView respawn and dismissal decay never learns. `"muted"` is the explicit
 *  "Mute this pattern" — straight to the 7-day mute, no ladder counting. */
export function recordFeedback(
  id: string,
  kind: "clicked" | "dismissed" | "muted" | "expired" | "up" | "down",
): Promise<void> {
  return invoke<void>("record_feedback", { id, kind });
}

/** Global bubble snooze (ADR-040/Q95): silences bubbles while capture +
 *  learning continue. Rendered as the HUD's 🔕 control. */
export function setSnooze(mode: "off" | "15m" | "1h" | "forever"): Promise<void> {
  return invoke<void>("set_snooze", { mode });
}

/** The snooze deadline (epoch ms; 0 = off, i64::MAX ≈ forever) — the HUD's 🔕
 *  control renders the truth, not its last click. */
export function getSnooze(): Promise<number> {
  return invoke<number>("get_snooze");
}

/** Dismiss the current voice surface on EVERY overlay window (multi-monitor
 *  convergence — a local-state dismiss left clones on the other monitors). */
export function voiceDismiss(): Promise<void> {
  return invoke("voice_dismiss");
}

/** Give this overlay window OS keyboard focus (non-exclusive panels opened
 *  without a click otherwise can't receive Escape/Tab/typing). */
export function focusOverlay(): Promise<void> {
  return invoke("focus_overlay");
}

/** Open the Activity & Privacy panel on THIS window's monitor (decision #13:
 *  controls follow the cursor — the calling window is where the click was).
 *  The core broadcasts `privacy_open {target}`; any copy open on another
 *  monitor closes. Callable from any monitor's bubble overflow / HUD. */
export function openPrivacy(): Promise<void> {
  return invoke("open_privacy");
}

/** Open the Dashboard on THIS window's monitor (HUD ◎ — decision #13). Same
 *  routing + single-instance contract as `openPrivacy`. */
export function openDashboard(): Promise<void> {
  return invoke("open_dashboard");
}

/** Tell the core this window's Context-Preview panel opened/closed (decision
 *  #13). Open makes this window THE preview host — core-staged MCP requests
 *  route (and queue) here — and broadcasts `preview_claimed` so every other
 *  window folds its copy. Call on every panel open/close transition. */
export function setPreviewHost(open: boolean): Promise<void> {
  return invoke("set_preview_host", { open });
}

/** Ask the core to BUILD a Context Payload for preview (doc 03 §4). The returned
 *  object IS the thing that will ship — the panel renders/edits it in place. */
export function requestPreview(intent: Intent, seedActionRef?: string): Promise<ContextPayload> {
  return invoke<ContextPayload>("request_preview", { intent, seedActionRef: seedActionRef ?? null });
}

/** The event-trail slice for the "Add more history" slider (doc 11 §4,
 *  ADR-040/Q71): metadata rows from the last N minutes, oldest-first, capped
 *  at EVENT_TRAIL_MAX. The panel swaps its `event_trail` item for this. */
export function listTrailEvents(minutes: number): Promise<unknown[]> {
  return invoke<unknown[]>("list_trail_events", { minutes });
}

/** Health of one transport for the preview footer's dot (doc 11 §4). */
export function transportHealth(target: TransportTarget): Promise<Health> {
  return invoke<Health>("transport_health", { target });
}

/** The approval gate's answer: the core-owned payload after the edits were
 *  synced and RE-REDACTED. `changed: true` means redaction altered content the
 *  user has not seen — approval was refused; re-render and ask again. */
export interface ApprovalResult {
  payload: ContextPayload;
  changed: boolean;
}

/** Approve a previewed payload (contract law: ONLY the preview panel may do
 *  this — doc 15 §2b). Sends the exact edited object; the core syncs it,
 *  re-runs redaction, and binds the approval to a content hash. */
export function previewSetApproved(payload: ContextPayload): Promise<ApprovalResult> {
  return invoke<ApprovalResult>("preview_set_approved", { payload });
}

/** Mirror of the `preview_send` result (internally tagged by `kind`). The push
 *  path is BOUND to the transport the preview named (SDLC review 2026-08-19
 *  finding 1, copying the MCP release gate): when `named` is not ready the
 *  core sends NOTHING and reports the mismatch instead of silently egressing
 *  over the next transport in the order — which may be the metered API key.
 *  `available` is the transport the core would pick instead, or null when no
 *  push transport is ready at all. */
export type PreviewSendResult =
  | { kind: "sent"; suggestions: StructuredSuggestions }
  | { kind: "transport_mismatch"; named: TransportTarget; available: TransportTarget | null };

/** Transmit the approved payload (doc 11 §4, doc 03 §4 — SHA-256 of the wire
 *  bytes is audit-logged as `cloud_send`). Takes only the id: the bytes that
 *  ship are the core-owned object bound at approval — preview == wire. On
 *  `transport_mismatch` the approval stays held core-side; see
 *  `previewRetarget`. */
export function previewSend(payloadId: string): Promise<PreviewSendResult> {
  return invoke<PreviewSendResult>("preview_send", { payloadId });
}

/** Re-stamp a previewed payload's `transport_target` after a
 *  `transport_mismatch` (finding 1). The core DROPS the approval: the user
 *  must press Send again, and that press re-approves — the explicit second
 *  confirmation the footer's changed transport label now backs. Returns the
 *  updated payload, which the panel swaps in through `onChange`. */
export function previewRetarget(
  payloadId: string,
  target: TransportTarget,
): Promise<ContextPayload> {
  return invoke<ContextPayload>("preview_retarget", { payloadId, target });
}

/** Reset this window's interactivity to click-through. The UI root calls this
 *  once on mount so a reload can never orphan a modal override. */
export function resetOverlayInteractivity(): Promise<void> {
  return invoke("reset_overlay_interactivity");
}

/** PTT pressed (doc 07): begin holding the mic. */
export function voicePttDown(): Promise<void> {
  return invoke("voice_ptt_down");
}

/** PTT released (doc 07): stop the mic, enqueue the STT job. */
export function voicePttUp(): Promise<void> {
  return invoke("voice_ptt_up");
}

/** Confirm-chip Run (doc 07 §4.4): re-issue the confirmed transcript through
 *  the query path at confidence 1.0. Never re-stores the utterance. */
export function voiceRunTranscript(transcript: string): Promise<void> {
  return invoke("voice_run_transcript", { transcript });
}

/** Cancel a preview: the core drops its in-process session — zero residue
 *  (doc 13 §3). Call on panel close without Send. */
export function previewCancel(payloadId: string): Promise<void> {
  return invoke("preview_cancel", { payloadId });
}

/** Read settings (the `ui`/`reasoning` blocks at minimum). */
export function getSettings(): Promise<Settings> {
  return invoke<Settings>("get_settings");
}

// ---------------------------------------------------------------------------
// Dashboard — read-only views over the local history ("what has it captured,
// what does it know"). Local DB only; nothing here egresses.
// ---------------------------------------------------------------------------

/** Aggregate counts + storage facts for the dashboard Overview. */
export interface DashboardStats {
  events: number;
  ocr_texts: number;
  embeddings: number;
  patterns: number;
  suggestions: number;
  connector_states: number;
  voice_utterances: number;
  sessions: number;
  first_event_ts: number | null;
  last_event_ts: number | null;
  db_bytes: number;
  db_encrypted: boolean;
  capture_enabled: boolean;
  voice_opt_in: boolean;
}

export function dashboardStats(): Promise<DashboardStats> {
  return invoke<DashboardStats>("dashboard_stats");
}

/** One history row: an event joined with its screen context (OCR excerpt). */
export interface HistoryEvent {
  id: number;
  ts: number;
  type: string;
  app: string | null;
  process: string | null;
  title: string | null;
  session_id: number | null;
  redaction_flags: number;
  payload: Record<string, unknown> | null;
  ocr: string | null;
  vlm_summary: string | null;
}

export function listEvents(opts?: {
  limit?: number;
  kind?: string;
  search?: string;
}): Promise<HistoryEvent[]> {
  return invoke<HistoryEvent[]>("list_events", {
    limit: opts?.limit ?? null,
    kind: opts?.kind ?? null,
    search: opts?.search ?? null,
  });
}

/** One mined pattern row (doc 08). */
export interface PatternRow {
  id: number;
  signature: string | null;
  n: number | null;
  support: number | null;
  confidence: number | null;
  last_seen: number | null;
  dismiss_decay: number | null;
  muted_until: number | null;
}

export function listPatterns(limit?: number): Promise<PatternRow[]> {
  return invoke<PatternRow[]>("list_patterns", { limit: limit ?? null });
}

/** One suggestion lifecycle row — includes resolved/expired ones. */
export interface SuggestionHistoryRow {
  id: number;
  title: string | null;
  glyph: string | null;
  confidence: number | null;
  state: string | null;
  shown_ts: number | null;
  resolved_ts: number | null;
  outcome: string | null;
  useful_rating: string | null;
  source: string | null;
}

export function listSuggestionHistory(limit?: number): Promise<SuggestionHistoryRow[]> {
  return invoke<SuggestionHistoryRow[]>("list_suggestion_history", { limit: limit ?? null });
}

/** Persist a (partial) settings patch. */
export function setSettings(patch: Settings): Promise<void> {
  return invoke("set_settings", { patch });
}

/** Is start-at-login registered with Windows (the registry truth)? */
export function getAutostart(): Promise<boolean> {
  return invoke<boolean>("get_autostart");
}

/** One VLM weight artifact's install state (decision #30). `present` means the
 *  file exists at the spawner's resolved path AT its expected byte size — a
 *  wrong-size file honestly reads as missing (corrupt/partial). */
export interface VlmArtifactStatus {
  file: string;
  expected_bytes: number;
  present: boolean;
}

/** The VLM install picture the Overview's notice renders (decision #30):
 *  without the ~3.3 GB weights the app silently ran OCR-only — now the state
 *  is visible and fixable in place. */
export interface VlmStatus {
  installed: boolean;
  downloading: boolean;
  missing_bytes: number;
  files: VlmArtifactStatus[];
}

export function vlmStatus(): Promise<VlmStatus> {
  return invoke<VlmStatus>("vlm_status");
}

/** Start the VLM weight download (decision #30). USER-INITIATED ONLY — this is
 *  the app's one sanctioned model-ingress path and it must never be called
 *  without an explicit click (no surprise network activity). Progress streams
 *  on the `vlm_fetch` event; the command returns immediately. */
export function vlmDownload(): Promise<void> {
  return invoke("vlm_download");
}

/** Register/unregister start-at-login; the choice persists as `ui.autostart`. */
export function setAutostart(on: boolean): Promise<void> {
  return invoke("set_autostart", { on });
}

// ---------------------------------------------------------------------------
// M9 — privacy surface (doc 13). Consent, the audit trail, exclusions, purge.
// ---------------------------------------------------------------------------

/** Mirror of the `get_consent` response (doc 13 §8). `db_encrypted` is reported
 *  by the core rather than assumed: the UI must never claim at-rest encryption
 *  the build did not actually apply. */
export interface ConsentState {
  first_run_completed: boolean;
  capture_enabled: boolean;
  voice_opt_in: boolean;
  capture_opt_in_ts: number | null;
  db_encrypted: boolean;
}

/** The four exclusion match kinds (doc 13 §4, ADR-040). */
export type ExclusionKind = "process" | "window_class" | "title_regex" | "url_pattern";

/** Mirror of one `exclusion_list` row. */
export interface ExclusionRow {
  id: number;
  match_kind: ExclusionKind;
  pattern: string;
  enabled: boolean;
}

/** Mirror of `detect_suggest::SuggestedExclusion` — a *candidate* the user
 *  confirms. Nothing is auto-excluded (ADR-029/Q15). */
export interface SuggestedExclusion {
  match_kind: ExclusionKind;
  pattern: string;
  label: string;
  reason: string;
}

/** One audit row: `capture_toggle` ("when was it watching?") or `cloud_send`
 *  ("what left this machine?") — doc 13 §3. */
export interface AuditRow {
  id: number;
  ts: number;
  type: "capture_toggle" | "cloud_send";
  payload: Record<string, unknown>;
}

/** Let a modal overlay surface accept clicks + keyboard (doc 11 §2, M9).
 *
 *  The overlay window is click-through and unfocusable by default — correct for
 *  passive bubbles, fatal for a dialog. Any modal surface MUST call this `true`
 *  on mount and `false` on unmount, or its buttons silently do nothing. */
export function setOverlayInteractive(interactive: boolean): Promise<void> {
  return invoke("set_overlay_interactive", { interactive });
}

/** One interactive region, physical px relative to this window (doc 11 §2). */
export interface HitRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Publish the overlay's interactive regions. The core's cursor poller makes
 *  the window accept input only while the cursor is inside one of these —
 *  everywhere else stays click-through. Empty array = fully click-through. */
export function setHitTestRects(rects: HitRect[]): Promise<void> {
  return invoke("set_hit_test_rects", { rects });
}

/** Current consent + whether the DB is really encrypted (doc 13 §6, §8). */
export function getConsent(): Promise<ConsentState> {
  return invoke<ConsentState>("get_consent");
}

/** Finish the first-run sequence with the user's explicit capture decision.
 *  Declining is a first-class outcome — first-run still completes. */
export function completeFirstRun(enableCapture: boolean): Promise<void> {
  return invoke("complete_first_run", { enableCapture });
}

/** Microphone opt-in at first PTT (doc 13 §8). */
export function grantVoiceConsent(): Promise<void> {
  return invoke("grant_voice_consent");
}

/** The Activity & Privacy audit feed, newest first (doc 13 §3, §7). */
export function listAudit(limit?: number): Promise<AuditRow[]> {
  return invoke<AuditRow[]>("list_audit", { limit: limit ?? null });
}

/** One-click Purge All (doc 13 §7). Confirm in the UI BEFORE calling this — it
 *  is irreversible. Returns the number of history rows deleted. */
export function purgeAll(): Promise<number> {
  return invoke<number>("purge_all");
}

/** The user's exclusion rules (doc 13 §4). */
export function listExclusions(): Promise<ExclusionRow[]> {
  return invoke<ExclusionRow[]>("list_exclusions");
}

/** Add an exclusion rule — "exclude this app/domain" (doc 13 §4, §9). */
export function addExclusion(matchKind: ExclusionKind, pattern: string): Promise<number> {
  return invoke<number>("add_exclusion", { matchKind, pattern });
}

/** Enable/disable (`enabled` boolean) or delete (`enabled: null`) one rule. */
export function setExclusion(id: number, enabled: boolean | null): Promise<void> {
  return invoke("set_exclusion", { id, enabled });
}

/** First-run detect-and-suggest (doc 13 §4, §8): local scan → candidates the
 *  user confirms. Applying one is a separate, explicit `addExclusion` call. */
export function suggestExclusions(): Promise<SuggestedExclusion[]> {
  return invoke<SuggestedExclusion[]>("suggest_exclusions");
}

// ---------------------------------------------------------------------------
// Typed event listeners. Each returns the tauri `UnlistenFn` so callers can
// clean up in a React effect.
// ---------------------------------------------------------------------------

function on<T>(name: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(name, (e: TauriEvent<T>) => handler(e.payload));
}

export const onBubbleSpec = (h: (e: BubbleSpecEvent) => void) =>
  on<BubbleSpecEvent>("bubble_spec", h);

export const onGpuBusy = (h: (busy: GpuBusyEvent) => void) =>
  on<GpuBusyEvent>("gpu_busy", h);

export const onCaptureIndicator = (h: (e: CaptureIndicatorEvent) => void) =>
  on<CaptureIndicatorEvent>("capture_indicator", h);

export const onVoiceSurface = (h: (e: VoiceSurfaceEvent) => void) =>
  on<VoiceSurfaceEvent>("voice_surface", h);

export const onSuggestionLifecycle = (h: (e: SuggestionLifecycleEvent) => void) =>
  on<SuggestionLifecycleEvent>("suggestion_lifecycle", h);

/** `"suggestion_rated"` — a thumb was recorded; every window sets it. */
export const onSuggestionRated = (h: (e: SuggestionRatedEvent) => void) =>
  on<SuggestionRatedEvent>("suggestion_rated", h);

/** `"settings_changed"` — re-read the settings you cached (decisions #7/#17). */
export const onSettingsChanged = (h: (e: SettingsChangedEvent) => void) =>
  on<SettingsChangedEvent>("settings_changed", h);

/** `"audit_alert"` — an audit write failed (decision #41); the primary overlay
 *  shows the dismissible warning banner. */
export const onAuditAlert = (h: (e: AuditAlertEvent) => void) =>
  on<AuditAlertEvent>("audit_alert", h);

/** `"suggestions_refresh"` — the snooze lifted; re-pull `list_suggestions` so
 *  the rows queued while it was on actually surface (finding 2). */
export const onSuggestionsRefresh = (h: (e: SuggestionsRefreshEvent) => void) =>
  on<SuggestionsRefreshEvent>("suggestions_refresh", h);

/** `"vlm_fetch"` — VLM weight download progress (decision #30); the Dashboard
 *  Overview renders the progress bar + terminal state. */
export const onVlmFetch = (h: (e: VlmFetchEvent) => void) =>
  on<VlmFetchEvent>("vlm_fetch", h);

/** `"dashboard_open"` — the tray (left-click / menu), a second app launch, or
 *  the HUD asks the `target` window to open the Dashboard; every other window
 *  closes its copy (decision #13). */
export const onDashboardOpen = (h: (target: string) => void) =>
  on<ControlOpenEvent>("dashboard_open", (e) => h(e.target));

/** `"privacy_open"` — a bubble's "Exclusions…" / the HUD asks the `target`
 *  window to open the Activity & Privacy panel; every other window closes its
 *  copy (decision #13). */
export const onPrivacyOpen = (h: (target: string) => void) =>
  on<ControlOpenEvent>("privacy_open", (e) => h(e.target));

/** `"preview_claimed"` — the `target` window's preview panel just opened; any
 *  OTHER window folds its own panel, cancelling its sessions core-side, so
 *  exactly one preview exists across monitors (decision #13). */
export const onPreviewClaimed = (h: (target: string) => void) =>
  on<ControlOpenEvent>("preview_claimed", (e) => h(e.target));

/** `"preview_request"` — the core staged a payload (MCP gated search, ADR-037)
 *  and asks this window to open the preview panel on it. Targeted at the
 *  current preview-host window, else the cursor's monitor (decision #13). The
 *  user's explicit "Approve for Claude" is the ONLY way its content ever
 *  leaves. */
export const onPreviewRequest = (h: (p: ContextPayload) => void) =>
  on<ContextPayload>("preview_request", h);

// ---------------------------------------------------------------------------
// v2 agent execution layer (Doc 22 §9, owner decisions #47–#54) — 2026-08-22.
// The loop itself rides the MCP gate (Claude Desktop ↔ aperture_agent_step);
// these are the USER's controls and the live view the status bar renders.
// ---------------------------------------------------------------------------

export type AgentActionType =
  | "click" | "type" | "key" | "launch" | "switch_window" | "scroll" | "wait" | "none";

export interface AgentAction {
  type: AgentActionType;
  target?: string | null;
  value?: string | null;
  direction?: "up" | "down" | "left" | "right" | null;
  amount?: number | null;
}

/** Why the loop is waiting on the user (mirrors `agent_loop::PauseReason`). */
export type AgentPause =
  | { kind: "approval" }
  | { kind: "confirm"; reason: string; action: AgentAction }
  | { kind: "clarification"; question: string }
  | { kind: "excluded"; label: string }
  | { kind: "elevated"; window: string }
  | { kind: "vram" };

export interface AgentStepLogEntry {
  step_number: number;
  summary: string;
  result: "success" | "failure" | "skipped" | string;
  reversibility: "reversible" | "irreversible" | "unknown" | string;
  ts: number;
}

export type AgentTaskState = "idle" | "running" | "paused" | "complete" | "failed" | "cancelled";

/** The `agent_task` event payload; `null` = no task (surface unmounts). */
export interface AgentTaskView {
  task_id: string;
  description: string;
  source: "claude" | "user";
  state: AgentTaskState;
  step: number;
  step_cap: number;
  pause: AgentPause | null;
  log: AgentStepLogEntry[];
  outcome: string | null;
  stop_reason: string | null;
  undoable_windows: { hwnd: number; title: string }[];
  in_flight: boolean;
}

export type AgentDecision = "approve" | "deny" | "confirm" | "skip" | "resume" | "stop";

export const agentStatus = () => invoke<AgentTaskView | null>("agent_status");
export const agentStartTask = (description: string) =>
  invoke<AgentTaskView>("agent_start_task", { description });
export const agentDecide = (taskId: string, decision: AgentDecision) =>
  invoke<AgentTaskView>("agent_decide", { taskId, decision });
export const agentAnswer = (taskId: string, answer: string) =>
  invoke<AgentTaskView>("agent_answer", { taskId, answer });
export const agentUndoCloseWindows = (taskId: string) =>
  invoke<number>("agent_undo_close_windows", { taskId });
export const agentDismiss = () => invoke<void>("agent_dismiss");

export interface AgentTaskRow {
  id: string;
  description: string;
  status: AgentTaskState;
  created_at: number;
  completed_at: number | null;
  step_count: number;
  outcome_summary: string | null;
}
export interface AgentStepRow {
  step_number: number;
  screen_payload_hash: string | null;
  action_type: string | null;
  action_target: string | null;
  action_value: string | null;
  result: string | null;
  claude_reasoning: string | null;
  timestamp: number;
}
export const agentListTasks = (limit?: number) =>
  invoke<AgentTaskRow[]>("agent_list_tasks", { limit: limit ?? null });
export const agentTaskSteps = (taskId: string) =>
  invoke<AgentStepRow[]>("agent_task_steps", { taskId });
export const agentPurgeTask = (taskId: string) => invoke<void>("agent_purge_task", { taskId });

export const onAgentTask = (h: (view: AgentTaskView | null) => void) =>
  on<AgentTaskView | null>("agent_task", h);

export type { UnlistenFn };
