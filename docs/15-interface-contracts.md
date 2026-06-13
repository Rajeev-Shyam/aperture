# Doc 15 — Interface Contracts

Five contracts make every subsystem independently buildable and testable. Components may depend on these and nothing else across subsystem boundaries.

## 1. Contract 1 — Event envelope (the bus + the DB row)
```rust
struct Event { id: i64, ts: i64, r#type: EventType, app: Option<String>,
               process: Option<String>, window_title: Option<String>,
               payload: serde_json::Value,            // type-specific (Doc 03 §2)
               connector_id: Option<String>, session_id: Option<i64>,
               redaction_flags: u32 }
```
Transport: in-process `tokio::sync::broadcast` in the Rust core; Tauri `invoke`/events bridge core ↔ WebView; **SQLite is the durable form** — the bus is at-most-once, the DB is the truth. Versioning: `payload` is additive-only; consumers ignore unknown fields.

## 2. Contract 2 — Context Payload
The JSON Schema in Doc 03 §4 (`aperture/context-payload/v1`). Invariants restated as contract law: (a) one object is built/previewed/sent; (b) only the preview panel may set `user_approved`; (c) only the gateway may consume an approved payload; (d) SHA-256 of the wire bytes is audit-logged.

## 3. Contract 3 — Connector trait
As Doc 10 §1 (`can_capture / capture / staleness_ttl / reconstruct / open / validate`), plus: `reconstruct_payload` JSON carries `payload_version`; migrations are per-connector pure functions `v(n)→v(n+1)`; `validate()` is mandatory for any cloud-suggested action to gain a button.

## 4. Contract 4 — GPU job
```rust
async fn enqueue(job: GpuJob) -> Result<JobOutput, JobError>
// GpuJob { kind: Vlm{image, prompt} | Stt{wav}, priority: u8, deadline, cancel }
// JobError: BudgetRefused{projection} | Deadline | Cancelled | SidecarDown
```
Law: callers never touch the GPU, a sidecar, or VRAM accounting; `BudgetRefused` carries the projection so callers can degrade intelligently (Doc 04 R3). `gpu_busy` is an observable broadcast derived from mutex state.

## 5. Contract 5 — ReasoningGateway + StructuredSuggestions
Trait per Doc 09 §2; output schema per Doc 09 §4. Law: **local candidates and cloud results flatten to the same `StructuredSuggestions` shape** before the Bubble UI sees them — the UI is source-agnostic except for a small "via Claude" source tag.

## 6. Compatibility & change rules
Additive-only fields everywhere; unknown-field tolerance mandatory; breaking changes require a new schema `$id`/`payload_version` plus a migration; every contract type lives in one shared `contracts` crate so drift is a compile error.

## 7. Test fakes (shipped with the contracts crate)
| Contract | Fake |
|---|---|
| Event envelope | scripted event-stream player (drives Doc 08 tests deterministically) |
| Context Payload | golden payloads incl. redaction fixtures |
| Connector | `FakeConnector` with programmable capture/reconstruct outcomes |
| GPU job | `FakeScheduler` with controllable latency/refusals (tests degrade ladders without a GPU) |
| Gateway | `FakeTransport` returning canned `StructuredSuggestions` / errors (tests the preview→send flow offline) |
