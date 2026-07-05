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

/** Mirror of `suggestions::BubbleSpec` (contracts/src/suggestions.rs).
 *  `action_ref` resolves to a connector on click (Critical Path B, doc 02 §5). */
export interface BubbleSpec {
  title: string;
  /** Connector-type glyph. */
  glyph: string;
  /** e.g. `"12:34 · 2h ago"`. */
  sublabel: string | null;
  action_ref: string;
  source: SuggestionSource;
  confidence: number;
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
  bubble_dwell_sec: number;
  max_glass_surfaces: number;
}

export interface Settings {
  ui?: UiSettings;
  reasoning?: {
    transport_order?: TransportTarget[];
    payload_size_warn_kb?: number;
    per_send_approval?: boolean;
  };
  // …other sections (capture/loadout/voice/pattern_engine/privacy) are passed
  // through untyped; the overlay only reads the `ui` + `reasoning` blocks.
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
  | { surface: "listening"; level?: number }
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
 *  WebView respawn and dismissal decay never learns. */
export function recordFeedback(
  id: string,
  kind: "clicked" | "dismissed" | "expired" | "up" | "down",
): Promise<void> {
  return invoke<void>("record_feedback", { id, kind });
}

/** Global bubble snooze (ADR-040/Q95): silences bubbles while capture +
 *  learning continue. TODO(M3-followup): render the snooze control
 *  (15 min / 1 h / until re-enabled) in the overlay's overflow menu. */
export function setSnooze(mode: "off" | "15m" | "1h" | "forever"): Promise<void> {
  return invoke<void>("set_snooze", { mode });
}

/** Ask the core to BUILD a Context Payload for preview (doc 03 §4). The returned
 *  object IS the thing that will ship — the panel renders/edits it in place. */
export function requestPreview(intent: Intent, seedActionRef?: string): Promise<ContextPayload> {
  return invoke<ContextPayload>("request_preview", { intent, seedActionRef: seedActionRef ?? null });
}

/** Set the in-process approval flag on a previewed payload (contract law: ONLY
 *  the preview panel may do this — doc 15 §2b). Sends the exact edited object. */
export function previewSetApproved(payload: ContextPayload): Promise<void> {
  // Contract law (doc 15 §2b): the sole setter of `user_approved`, which it always
  // flips true. Sends just the id; the core marks the in-process payload approved.
  return invoke("preview_set_approved", { payloadId: payload.payload_id });
}

/** Transmit the EXACT serialized payload bytes to the gateway (doc 11 §4,
 *  doc 03 §4 — SHA-256 of the wire bytes is audit-logged as `cloud_send`).
 *  Only the gateway, behind this command, may emit to the network. */
export function previewSend(payload: ContextPayload): Promise<StructuredSuggestions> {
  return invoke<StructuredSuggestions>("preview_send", { payload });
}

/** PTT pressed (doc 07): begin holding the mic. */
export function voicePttDown(): Promise<void> {
  return invoke("voice_ptt_down");
}

/** PTT released (doc 07): stop the mic, enqueue the STT job. */
export function voicePttUp(): Promise<void> {
  return invoke("voice_ptt_up");
}

/** Read settings (the `ui`/`reasoning` blocks at minimum). */
export function getSettings(): Promise<Settings> {
  return invoke<Settings>("get_settings");
}

/** Persist a (partial) settings patch. */
export function setSettings(patch: Settings): Promise<void> {
  return invoke("set_settings", { patch });
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

export type { UnlistenFn };
