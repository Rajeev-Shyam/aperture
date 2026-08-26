//! Dashboard (doc 11, ADR-040) — the "what has it captured, what does it know"
//  window: a simple, Claude-Desktop-shaped sidebar + content view over the
//  local history DB. Read-only; every byte shown already lives on this machine
//  and nothing here egresses (two-emitter rule untouched).
//
//  Opaque, never glass: alongside first-run and the privacy panel this is one
//  of the largest surfaces in the app (glass budget, doc 14 §5). Modal contract
//  via useModalSurface; Escape closes.

import { useEffect, useRef, useState } from "react";

import {
  agentListTasks,
  agentPurgeTask,
  agentTaskSteps,
  dashboardStats,
  getAutostart,
  getSettings,
  grantVoiceConsent,
  listEvents,
  listPatterns,
  listSuggestionHistory,
  onVlmFetch,
  recordFeedback,
  setAutostart,
  setSettings,
  transportHealth,
  vlmDownload,
  vlmStatus,
  type AgentStepRow,
  type AgentTaskRow,
  type DashboardStats,
  type Health,
  type HistoryEvent,
  type PatternEngineSettings,
  type PatternRow,
  type SuggestionHistoryRow,
  type TransportTarget,
  type VlmFetchEvent,
  type VlmStatus,
  type VoiceSettings,
} from "../lib/ipc";
import { useDraggable } from "../state/useDraggable";
import { useModalSurface } from "../state/useModalSurface";

type Tab = "overview" | "history" | "patterns" | "suggestions" | "voice" | "agent" | "advanced";

const TABS: { id: Tab; label: string; hint: string }[] = [
  { id: "overview", label: "Overview", hint: "What Aperture holds right now" },
  { id: "history", label: "History", hint: "Everything captured, searchable" },
  { id: "patterns", label: "Patterns", hint: "Habits the engine has mined" },
  { id: "suggestions", label: "Suggestions", hint: "Every bubble ever surfaced" },
  { id: "voice", label: "Voice", hint: "Push-to-talk transcripts" },
  { id: "agent", label: "Agent", hint: "Every agent task and each audited step" },
  { id: "advanced", label: "Advanced", hint: "Tuning: bubbles, habits, transport" },
];

interface Props {
  onClose: () => void;
  onOpenPrivacy: () => void;
}

export function Dashboard({ onClose, onOpenPrivacy }: Props) {
  const [tab, setTab] = useState<Tab>("overview");
  const panelRef = useRef<HTMLDivElement>(null);
  // Non-exclusive: the dashboard is clickable via its own rect; everything
  // outside it stays click-through to the user's apps.
  const trapKeys = useModalSurface(panelRef, { exclusive: false });
  // Window-style drag by the titlebar.
  const drag = useDraggable(panelRef);

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
      return;
    }
    trapKeys(e);
  }

  return (
    <div
      className="dash surface-opaque surface-interactive"
      role="dialog"
      aria-modal="true"
      aria-label="Aperture dashboard"
      ref={panelRef}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      style={drag.style}
    >
      <header className="dash__titlebar" {...drag.handleProps}>
        <span className="dash__brand">
          <span aria-hidden>◎</span> Aperture
        </span>
        <button className="btn btn--icon" aria-label="Close the dashboard" onClick={onClose}>
          ×
        </button>
      </header>
      <div className="dash__body">
        <nav className="dash__nav" aria-label="Dashboard sections">
          {TABS.map((t) => (
            <button
              key={t.id}
              className={`dash__navitem ${tab === t.id ? "dash__navitem--active" : ""}`}
              title={t.hint}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
          <div className="dash__navfoot">
            <button className="dash__navitem" onClick={onOpenPrivacy}>
              🛡 Activity &amp; Privacy
            </button>
          </div>
        </nav>
        <main className="dash__content">
          {tab === "overview" && <OverviewTab />}
          {tab === "history" && <HistoryTab kind={null} />}
          {tab === "patterns" && <PatternsTab />}
          {tab === "suggestions" && <SuggestionsTab />}
          {tab === "voice" && <VoiceTab />}
          {tab === "agent" && <AgentTab />}
          {tab === "advanced" && <AdvancedTab />}
        </main>
      </div>
    </div>
  );
}

// --- Overview ---------------------------------------------------------------

function OverviewTab() {
  const [stats, setStats] = useState<DashboardStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  // null = unknown (read failed / still loading): render the control disabled
  // rather than showing a checkbox state that may be a lie.
  const [autostart, setAutostartState] = useState<boolean | null>(null);

  useEffect(() => {
    void dashboardStats().then(setStats).catch((e) => setError(String(e)));
    void getAutostart().then(setAutostartState).catch(() => setAutostartState(null));
  }, []);

  async function toggleAutostart(on: boolean) {
    setAutostartState(on); // optimistic; revert on failure
    try {
      await setAutostart(on);
    } catch {
      setAutostartState(!on);
    }
  }

  if (error) return <p className="dash__error">{error}</p>;
  if (!stats) return <p className="dash__empty">Loading…</p>;

  const tiles: { label: string; value: string; sub?: string }[] = [
    { label: "Events captured", value: fmtCount(stats.events), sub: spanLabel(stats) },
    { label: "Screens read (OCR)", value: fmtCount(stats.ocr_texts) },
    { label: "Semantic embeddings", value: fmtCount(stats.embeddings) },
    { label: "Patterns mined", value: fmtCount(stats.patterns) },
    { label: "Suggestions surfaced", value: fmtCount(stats.suggestions) },
    { label: "Resumable states", value: fmtCount(stats.connector_states) },
    { label: "Voice utterances", value: fmtCount(stats.voice_utterances) },
    { label: "Work sessions", value: fmtCount(stats.sessions) },
    { label: "On disk", value: fmtBytes(stats.db_bytes), sub: "history.db, local only" },
  ];

  return (
    <div>
      <h2>Overview</h2>
      <p className="dash__lede">
        Everything below lives in one local file and never leaves this machine unless you
        explicitly approve a Send.
      </p>
      <div className="dash__tiles">
        {tiles.map((t) => (
          <div key={t.label} className="dash__tile">
            <div className="dash__tile-value">{t.value}</div>
            <div className="dash__tile-label">{t.label}</div>
            {t.sub && <div className="dash__tile-sub">{t.sub}</div>}
          </div>
        ))}
      </div>
      <p className="dash__facts">
        Capture is <strong>{stats.capture_enabled ? "on" : "off"}</strong>. At-rest encryption is{" "}
        <strong>{stats.db_encrypted ? "active" : "not active in this build"}</strong>.
      </p>
      <VlmSection />
      <label className="dash__setting">
        <input
          type="checkbox"
          checked={autostart ?? false}
          disabled={autostart === null}
          onChange={(e) => void toggleAutostart(e.target.checked)}
        />
        <span>
          Start Aperture when I sign in to Windows
          <span className="dash__setting-sub">
            Capture resumes exactly as you left it — the tray icon is always there to turn it off.
          </span>
        </span>
      </label>
    </div>
  );
}

function spanLabel(s: DashboardStats): string | undefined {
  if (!s.first_event_ts) return undefined;
  return `since ${new Date(s.first_event_ts).toLocaleDateString()}`;
}

// --- VLM install (decision #30) ---------------------------------------------
// The ~3.3 GB screen-understanding weights can't ship in the installer, and a
// machine without them used to degrade to OCR-only SILENTLY. This section makes
// the state honest: a visible notice + a strictly user-initiated download with
// real progress. Nothing here runs without the click.

function VlmSection() {
  const [status, setStatus] = useState<VlmStatus | null>(null);
  const [fetchEv, setFetchEv] = useState<VlmFetchEvent | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void vlmStatus().then(setStatus).catch((e) => setError(String(e)));
    let unlisten: (() => void) | undefined;
    void onVlmFetch((e) => {
      setFetchEv(e);
      if (e.phase === "error") setError(e.error ?? "download failed");
      // Terminal success: re-read so `installed` flips from the file truth.
      if (e.phase === "done") void vlmStatus().then(setStatus).catch(() => {});
    }).then((u) => {
      unlisten = u;
    });
    return () => unlisten?.();
  }, []);

  async function download() {
    setError(null);
    setFetchEv(null);
    try {
      await vlmDownload();
      setStatus((s) => (s ? { ...s, downloading: true } : s));
    } catch (e) {
      setError(String(e));
    }
  }

  if (!status) return null;

  if (status.installed) {
    return (
      <p className="dash__facts">
        Screen understanding (VLM) is <strong>installed</strong>
        {fetchEv?.phase === "done"
          ? " — it loads automatically the next time it's needed, no restart."
          : "."}
      </p>
    );
  }

  const downloading = fetchEv?.phase === "downloading" || (status.downloading && fetchEv?.phase !== "error");
  if (downloading) {
    const received = fetchEv?.phase === "downloading" ? fetchEv.received_bytes : 0;
    const total = fetchEv?.phase === "downloading" ? fetchEv.total_bytes : status.missing_bytes;
    const pct = total > 0 ? Math.floor((received / total) * 100) : 0;
    return (
      <div className="dash__banner dash__vlm">
        <p>Downloading screen understanding (VLM)…</p>
        <div
          className="dash__progress"
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={pct}
        >
          <div className="dash__progress-fill" style={{ width: `${pct}%` }} />
        </div>
        <p className="dash__progress-label">
          {pct}% · {fmtBytes(received)} of {fmtBytes(total)}
          {fetchEv?.phase === "downloading" && fetchEv.file ? ` · ${fetchEv.file}` : ""}
        </p>
      </div>
    );
  }

  return (
    <div className="dash__banner dash__vlm">
      <p>
        <strong>Screen understanding (VLM) is not installed</strong> — Aperture is running in
        OCR-only mode. Everything still works; on-screen scenes are just read as plain text
        instead of being understood. The model is a one-time download from Hugging Face and
        happens only when you click — nothing of yours is uploaded.
      </p>
      {error && <p className="dash__error">{error} — the download resumes where it stopped.</p>}
      <button className="btn btn--primary" onClick={() => void download()}>
        {error ? "Retry download" : `Download (${fmtBytes(status.missing_bytes)})`}
      </button>
    </div>
  );
}

// --- History (also serves the Voice tab, filtered) --------------------------

function HistoryTab({ kind }: { kind: string | null }) {
  const [rows, setRows] = useState<HistoryEvent[]>([]);
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const voice = kind === "voice_utterance";

  useEffect(() => {
    setLoading(true);
    const t = setTimeout(() => {
      void listEvents({ limit: 200, kind: kind ?? undefined, search: search || undefined })
        .then((r) => {
          setRows(r);
          setError(null);
        })
        .catch((e) => setError(String(e)))
        .finally(() => setLoading(false));
    }, 250);
    return () => clearTimeout(t);
  }, [kind, search]);

  return (
    <div>
      <h2>{voice ? "Voice" : "History"}</h2>
      <p className="dash__lede">
        {voice
          ? "Every push-to-talk utterance, transcribed and stored locally."
          : "The raw capture stream — window focus, navigation, media, documents — with the text read off your screen."}
      </p>
      <input
        className="dash__search"
        type="search"
        placeholder={voice ? "Search transcripts…" : "Search titles, apps, screen text…"}
        value={search}
        onChange={(e) => setSearch(e.target.value)}
      />
      {error && <p className="dash__error">{error}</p>}
      {!error && !loading && rows.length === 0 && (
        <p className="dash__empty">
          {voice
            ? "Nothing yet — enable voice above, then hold the push-to-talk chord and speak."
            : "Nothing captured yet. Turn capture on and use your machine for a bit."}
        </p>
      )}
      <ul className="dash__rows">
        {rows.map((r) => (
          <li key={r.id} className="dash__row">
            <div className="dash__row-head">
              <span className="dash__row-type">{r.type}</span>
              <span className="dash__row-app">{r.app ?? r.process ?? ""}</span>
              <time>{fmtTime(r.ts)}</time>
            </div>
            {voice ? (
              <div className="dash__row-title">
                {String(r.payload?.transcript ?? "(no transcript)")}
              </div>
            ) : (
              <>
                {r.title && <div className="dash__row-title">{r.title}</div>}
                {r.redaction_flags !== 0 && (
                  <div className="dash__row-excluded">excluded — metadata only</div>
                )}
                {r.ocr && <div className="dash__row-ocr">{r.ocr}</div>}
              </>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

// --- Voice: consent + how-to + transcripts -----------------------------------

/** Human label for the confirm floor (decision #27). */
function floorLabel(v: number): string {
  if (v >= 1) return "always confirm";
  if (v <= 0) return "never confirm";
  return `below ${Math.round(v * 100)}% confidence`;
}

function VoiceTab() {
  const [stats, setStats] = useState<DashboardStats | null>(null);
  const [chord, setChord] = useState<string>("Ctrl+Alt+Space");
  // null until the settings read lands: the control renders disabled rather
  // than showing (and potentially persisting over) a value that may be a lie.
  const [floor, setFloor] = useState<number | null>(null);
  // The whole voice section, kept so a floor write merges instead of clobbering
  // ptt_hotkey etc. (set_settings replaces top-level keys wholesale).
  const voiceSection = useRef<VoiceSettings>({});
  const [error, setError] = useState<string | null>(null);
  // One write per gesture (SDLC review 2026-08-19 finding 10, the AdvancedTab
  // fix applied here): a range input fires onChange for EVERY step of a drag,
  // and each write is a DB row + a `settings_changed` broadcast to every
  // window. The slider's last value waits 300 ms after the last step.
  const pendingFloor = useRef<number | null>(null);
  const flushTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  function refresh() {
    void dashboardStats().then(setStats).catch((e) => setError(String(e)));
    void getSettings()
      .then((s) => {
        const v = s.voice ?? {};
        voiceSection.current = v;
        if (v.ptt_hotkey) setChord(v.ptt_hotkey);
        // Clamp for display exactly like the core clamps at read (decision #27).
        const raw = typeof v.intent_confidence_floor === "number" ? v.intent_confidence_floor : 0.6;
        setFloor(Math.min(1, Math.max(0, raw)));
      })
      .catch(() => {});
  }

  useEffect(() => {
    refresh();
    // A gesture still in flight when the panel closes must not be lost.
    return () => {
      if (flushTimer.current) clearTimeout(flushTimer.current);
      flushFloor();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function enable() {
    try {
      await grantVoiceConsent();
      // The shell may rebind the chord if the configured one is taken; give it
      // a beat, then re-read what actually bound.
      setTimeout(refresh, 600);
    } catch (e) {
      setError(String(e));
    }
  }

  /** Write the pending floor, if any (decision #27): merge into the voice
   *  section so the write can't drop sibling keys. The core reads it per
   *  utterance — the change applies to the very next press, no restart. */
  function flushFloor() {
    const v = pendingFloor.current;
    if (v === null) return;
    pendingFloor.current = null;
    voiceSection.current = { ...voiceSection.current, intent_confidence_floor: v };
    void setSettings({ voice: voiceSection.current }).catch((e) => setError(String(e)));
  }

  /** Slider step: show it now, persist it once the gesture settles. */
  function updateFloor(v: number) {
    setFloor(v);
    pendingFloor.current = v;
    if (flushTimer.current) clearTimeout(flushTimer.current);
    flushTimer.current = setTimeout(flushFloor, 300);
  }

  return (
    <div>
      {stats && !stats.voice_opt_in && (
        <div className="dash__banner">
          <p>
            Voice needs its own opt-in: press-to-talk opens your microphone, and every
            utterance is transcribed and stored locally (never sent anywhere).
          </p>
          <button className="btn btn--primary" onClick={() => void enable()}>
            Enable voice (allow microphone)
          </button>
        </div>
      )}
      {stats?.voice_opt_in && (
        <p className="dash__facts">
          Voice is enabled{stats.capture_enabled ? "" : " (waiting for capture to be on)"} — hold{" "}
          <strong>{chord}</strong>, speak, release. Transcription runs locally (whisper on CPU);
          nothing you say leaves this machine.
        </p>
      )}
      {/* Confirm-before-acting floor (decision #27): a slider whose top stop is
          the "always confirm" 1.0 option. */}
      <label className="dash__setting dash__setting--slider">
        <input
          type="range"
          min={0}
          max={1}
          step={0.05}
          value={floor ?? 0.6}
          disabled={floor === null}
          aria-label="Confirmation threshold"
          onChange={(e) => updateFloor(Number(e.target.value))}
        />
        <span>
          Ask “Did you say…?” {floor === null ? "…" : floorLabel(floor)}
          <span className="dash__setting-sub">
            When the transcription is less certain than this, Aperture shows the transcript and
            waits for you instead of acting. Slide to 100% to always confirm first. Applies to
            the next press — no restart.
          </span>
        </span>
      </label>
      {error && <p className="dash__error">{error}</p>}
      <HistoryTab kind="voice_utterance" />
    </div>
  );
}

// --- Advanced: the knobs that were settings-only ------------------------------
// Everything here was already read by the core but had no control (decisions
// #7, #17, #39): the dwell was a compile-time constant, the pattern_engine
// block was re-read once a day, and the transport order was frozen at launch.
// `set_settings` now announces its write, so each of these applies live —
// that is the difference between a control and a next-launch preference.
//
// Every write MERGES its whole section: `set_settings` replaces top-level keys,
// so patching one field alone would drop its siblings.

/** One labelled slider over a settings number. */
function Knob({
  label,
  hint,
  min,
  max,
  step,
  value,
  format,
  onChange,
}: {
  label: string;
  hint: string;
  min: number;
  max: number;
  step: number;
  value: number | null;
  format: (v: number) => string;
  onChange: (v: number) => void;
}) {
  return (
    <label className="dash__setting dash__setting--slider">
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value ?? min}
        // null = the settings read has not landed. Disabled rather than
        // showing (and potentially persisting over) a value that may be a lie.
        disabled={value === null}
        aria-label={label}
        onChange={(e) => onChange(Number(e.target.value))}
      />
      <span>
        {label} — <strong>{value === null ? "…" : format(value)}</strong>
        <span className="dash__setting-sub">{hint}</span>
      </span>
    </label>
  );
}

/** The push transports, in the order the radio offers them. MCP is deliberately
 *  absent: it is pull-only (Claude Desktop asks Aperture), so it can never be
 *  what a Send uses — offering it as a "preferred transport" would be a lie. */
const PUSH_TRANSPORTS: { id: TransportTarget; label: string; hint: string }[] = [
  {
    id: "claude-cli",
    label: "Claude CLI",
    hint: "Runs the local `claude` binary. No API key, uses your CLI login.",
  },
  {
    id: "messages-api",
    label: "Messages API",
    hint: "Direct HTTPS to api.anthropic.com. Needs an API key in settings.",
  },
];

const ALL_TRANSPORTS: TransportTarget[] = [
  "claude-desktop-mcp",
  "claude-cli",
  "messages-api",
];

function healthLabel(h: Health | undefined): string {
  if (!h) return "checking…";
  if (h.kind === "ready") return "ready";
  return `${h.kind === "needs_setup" ? "needs setup" : "unavailable"} — ${h.detail}`;
}

function AdvancedTab() {
  const [dwellSec, setDwellSec] = useState<number | null>(null);
  const [order, setOrder] = useState<TransportTarget[] | null>(null);
  const [knobs, setKnobs] = useState<PatternEngineSettings | null>(null);
  const [health, setHealth] = useState<Partial<Record<TransportTarget, Health>>>({});
  const [error, setError] = useState<string | null>(null);

  // Edits waiting to be written, keyed by settings section. A range input fires
  // onChange for EVERY step of a drag, and each write is a DB row + a
  // broadcast to every window + a pattern-engine reconfigure — so the slider is
  // debounced into one write per gesture rather than one per pixel.
  const pending = useRef<Record<string, Record<string, unknown>>>({});
  const flushTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  function refresh() {
    void getSettings()
      .then((s) => {
        setDwellSec(typeof s.ui?.bubble_dwell_sec === "number" ? s.ui.bubble_dwell_sec : 20);
        setOrder(s.reasoning?.transport_order ?? null);
        setKnobs(s.pattern_engine ?? {});
      })
      .catch((e) => setError(String(e)));
  }

  function refreshHealth() {
    ALL_TRANSPORTS.forEach((t) => {
      void transportHealth(t)
        .then((h) => setHealth((cur) => ({ ...cur, [t]: h })))
        .catch(() => {});
    });
  }

  useEffect(() => {
    refresh();
    refreshHealth();
    // A gesture still in flight when the panel closes must not be lost.
    return () => {
      if (flushTimer.current) clearTimeout(flushTimer.current);
      void flush();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /**
   * Write every pending edit, merging each into the settings as they are RIGHT
   * NOW rather than into a copy read when this tab mounted.
   *
   * `set_settings` replaces a whole top-level key, and `ui` has a second writer
   * — the draggable HUD persists `ui.hud_anchor`. Merging into a mount-time
   * snapshot would silently revert whatever that writer did in between, which
   * is a lost update the user would experience as "my HUD jumped back".
   */
  async function flush() {
    const batch = pending.current;
    pending.current = {};
    if (Object.keys(batch).length === 0) return;
    try {
      const current = await getSettings();
      const patch: Record<string, unknown> = {};
      for (const [section, fields] of Object.entries(batch)) {
        const existing = (current[section] ?? {}) as Record<string, unknown>;
        patch[section] = { ...existing, ...fields };
      }
      await setSettings(patch as Parameters<typeof setSettings>[0]);
    } catch (e) {
      // Never swallowed: a control that silently failed to persist is worse
      // than one that says so — the user would keep trusting the slider.
      setError(String(e));
    }
  }

  /** Queue one field for the debounced write. */
  function queueSave(section: string, fields: Record<string, unknown>) {
    setError(null);
    pending.current[section] = { ...(pending.current[section] ?? {}), ...fields };
    if (flushTimer.current) clearTimeout(flushTimer.current);
    flushTimer.current = setTimeout(() => void flush(), 300);
  }

  function updateDwell(sec: number) {
    setDwellSec(sec);
    queueSave("ui", { bubble_dwell_sec: sec });
  }

  function updateKnob(key: keyof PatternEngineSettings, v: number) {
    setKnobs((cur) => ({ ...(cur ?? {}), [key]: v }));
    queueSave("pattern_engine", { [key]: v });
  }

  /** Decision #39: move `pick` to the front of the order. MCP keeps its place
   *  in the list — dropping it would silently unregister the pull path the
   *  Claude Desktop flow uses; only the PUSH preference changes here. */
  function preferTransport(pick: TransportTarget) {
    const current = order ?? ALL_TRANSPORTS;
    const next = [pick, ...current.filter((t) => t !== pick)];
    setOrder(next);
    // A radio is one click, not a drag — write it straight through so the
    // gateway rebuild (and the health re-probe below) are not waiting on a
    // debounce the user cannot see.
    pending.current.reasoning = { ...(pending.current.reasoning ?? {}), transport_order: next };
    void flush().then(() => {
      // The core rebuilds the gateway on that write; re-probe so the dots
      // describe the transports as they are NOW composed.
      refreshHealth();
    });
  }

  // The transport a Send would actually use: first PUSH entry in the order.
  const activePush =
    (order ?? ALL_TRANSPORTS).find((t) => t !== "claude-desktop-mcp") ?? "claude-cli";

  return (
    <div>
      <h2>Advanced</h2>
      <p className="dash__lede">
        Tuning for how often Aperture speaks up and where “Ask Claude” sends things. Every
        control here applies immediately — no restart.
      </p>
      {error && <p className="dash__error">{error}</p>}

      <h3 className="dash__subhead">Bubbles</h3>
      <Knob
        label="How long a bubble stays"
        hint="Hovering pauses the countdown, so this is the time it waits while you are not looking at it."
        min={5}
        max={120}
        step={1}
        value={dwellSec}
        format={(v) => `${v}s`}
        onChange={updateDwell}
      />

      <h3 className="dash__subhead">When Aperture speaks up</h3>
      <p className="dash__facts">
        These tune the habit engine directly. Out-of-range values are ignored by the core in
        favour of the shipped defaults, so a bad number can never make it noisier than its
        built-in ceiling.
      </p>
      <Knob
        label="Certainty before suggesting"
        hint="Higher means fewer, more confident bubbles. Shipped default: 70%."
        min={0.5}
        max={0.95}
        step={0.05}
        value={typeof knobs?.tau_conf === "number" ? knobs.tau_conf : knobs ? 0.7 : null}
        format={(v) => `${Math.round(v * 100)}%`}
        onChange={(v) => updateKnob("tau_conf", v)}
      />
      <Knob
        label="Repeats before it counts as a habit"
        hint="How many times you must repeat a sequence before it can produce a bubble. Shipped default: 3."
        min={2}
        max={10}
        step={1}
        value={
          typeof knobs?.cold_start_support_floor === "number"
            ? knobs.cold_start_support_floor
            : knobs
              ? 3
              : null
        }
        format={(v) => `${v}×`}
        onChange={(v) => updateKnob("cold_start_support_floor", v)}
      />
      <Knob
        label="Quiet time per habit"
        hint="The same habit will not surface again inside this window. Shipped default: 30 minutes."
        min={5}
        max={180}
        step={5}
        value={typeof knobs?.cooldown_min === "number" ? knobs.cooldown_min : knobs ? 30 : null}
        format={(v) => `${v} min`}
        onChange={(v) => updateKnob("cooldown_min", v)}
      />
      <Knob
        label="Suggestions per hour"
        hint="The starting budget. Aperture opens or closes it on its own between 2 and 8 depending on whether you click them."
        min={2}
        max={8}
        step={1}
        value={
          typeof knobs?.cap_per_hour_default === "number"
            ? knobs.cap_per_hour_default
            : knobs
              ? 4
              : null
        }
        format={(v) => `${v}/hr`}
        onChange={(v) => updateKnob("cap_per_hour_default", v)}
      />

      <h3 className="dash__subhead">Where “Ask Claude” sends things</h3>
      <p className="dash__facts">
        Nothing is sent until you approve it in the preview — this only decides the route it
        takes once you do.
      </p>
      {PUSH_TRANSPORTS.map((t) => (
        <label key={t.id} className="dash__setting">
          <input
            type="radio"
            name="push-transport"
            checked={activePush === t.id}
            disabled={order === null}
            onChange={() => preferTransport(t.id)}
          />
          <span>
            {t.label} <span className="dash__health">· {healthLabel(health[t.id])}</span>
            <span className="dash__setting-sub">{t.hint}</span>
          </span>
        </label>
      ))}
      <p className="dash__facts">
        Claude Desktop (MCP) is <strong>{healthLabel(health["claude-desktop-mcp"])}</strong>. It
        is a pull route — Claude Desktop asks Aperture for context and you approve the request
        — so it is never what a Send uses, and it stays available regardless of the choice
        above.
      </p>
    </div>
  );
}

// --- Patterns ----------------------------------------------------------------

function PatternsTab() {
  const [rows, setRows] = useState<PatternRow[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void listPatterns(200).then(setRows).catch((e) => setError(String(e)));
  }, []);

  return (
    <div>
      <h2>Patterns</h2>
      <p className="dash__lede">
        Recurring habits the engine has mined from your activity — the source of proactive
        suggestions. Confidence is recency-weighted; dismissing bubbles decays it.
      </p>
      {error && <p className="dash__error">{error}</p>}
      {!error && rows.length === 0 && (
        <p className="dash__empty">
          Nothing mined yet — patterns need a few repetitions of the same sequence.
        </p>
      )}
      <ul className="dash__rows">
        {rows.map((p) => (
          <li key={p.id} className="dash__row">
            <div className="dash__row-head">
              <span className="dash__row-type">
                {(p.confidence ?? 0).toFixed(2)} conf · ×{p.support ?? 0}
              </span>
              {p.muted_until && <span className="dash__row-excluded">muted</span>}
              {p.last_seen && <time>{fmtTime(p.last_seen)}</time>}
            </div>
            <div className="dash__row-title">
              <code>{p.signature ?? "(unnamed)"}</code>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

// --- Suggestions -------------------------------------------------------------

function SuggestionsTab() {
  const [rows, setRows] = useState<SuggestionHistoryRow[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void listSuggestionHistory(200).then(setRows).catch((e) => setError(String(e)));
  }, []);

  /** Record a "useful?" rating (SC7's signal) and echo it locally. */
  async function rate(id: number, kind: "up" | "down") {
    try {
      await recordFeedback(String(id), kind);
      setRows((cur) => cur.map((r) => (r.id === id ? { ...r, useful_rating: kind } : r)));
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div>
      <h2>Suggestions</h2>
      <p className="dash__lede">
        Every bubble Aperture has surfaced, and what happened to it. This is also what the
        engine learns from.
      </p>
      {error && <p className="dash__error">{error}</p>}
      {!error && rows.length === 0 && (
        <p className="dash__empty">No suggestions yet — they appear once patterns form.</p>
      )}
      <ul className="dash__rows">
        {rows.map((s) => (
          <li key={s.id} className="dash__row">
            <div className="dash__row-head">
              <span className="dash__row-type">
                {s.state ?? "?"}
                {s.outcome ? ` · ${s.outcome}` : ""}
              </span>
              {/* The "useful?" thumbs (SC7, Q81) — ratable here after the
                  bubble is gone, so late judgments still count. */}
              <span className="dash__row-thumbs">
                <button
                  className={`dash__thumb ${s.useful_rating === "up" ? "dash__thumb--set" : ""}`}
                  aria-label="Rate useful"
                  aria-pressed={s.useful_rating === "up"}
                  onClick={() => void rate(s.id, "up")}
                >
                  👍
                </button>
                <button
                  className={`dash__thumb ${s.useful_rating === "down" ? "dash__thumb--set" : ""}`}
                  aria-label="Rate not useful"
                  aria-pressed={s.useful_rating === "down"}
                  onClick={() => void rate(s.id, "down")}
                >
                  👎
                </button>
              </span>
              {s.shown_ts && <time>{fmtTime(s.shown_ts)}</time>}
            </div>
            <div className="dash__row-title">
              {s.glyph} {s.title ?? "(untitled)"}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

// --- shared formatters -------------------------------------------------------

function fmtCount(n: number): string {
  return n >= 10_000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

function fmtBytes(b: number): string {
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(1)} MB`;
  return `${(b / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function fmtTime(ts: number): string {
  const d = new Date(ts);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  return sameDay
    ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

// --- Agent (v2, Doc 22 §9 / V2-M6) -------------------------------------------

/** Task history + the per-step audit trail (payload hash, action, result). */
function AgentTab() {
  const [rows, setRows] = useState<AgentTaskRow[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [steps, setSteps] = useState<AgentStepRow[]>([]);
  const [error, setError] = useState<string | null>(null);

  const reload = () => agentListTasks(100).then(setRows).catch((e) => setError(String(e)));
  useEffect(() => {
    void reload();
  }, []);

  async function toggle(id: string) {
    if (open === id) {
      setOpen(null);
      return;
    }
    try {
      setSteps(await agentTaskSteps(id));
      setOpen(id);
    } catch (e) {
      setError(String(e));
    }
  }

  async function purge(id: string) {
    try {
      await agentPurgeTask(id);
      if (open === id) setOpen(null);
      await reload();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div>
      <h2>Agent tasks</h2>
      <p className="dash__lede">
        Every task Claude has driven on this PC, and every step it took — what was sent
        (as a hash), what it did, and what happened. "Purge" deletes a task and its steps.
      </p>
      {error && <p className="dash__error">{error}</p>}
      {!error && rows.length === 0 && (
        <p className="dash__empty">No agent tasks yet — press ✦ on the overlay, or ask Claude Desktop.</p>
      )}
      <ul className="dash__rows">
        {rows.map((t) => (
          <li key={t.id} className="dash__row">
            <div className="dash__row-head">
              <span className="dash__row-type">
                {t.status} · {t.step_count} step{t.step_count === 1 ? "" : "s"}
              </span>
              <time dateTime={new Date(t.created_at).toISOString()}>
                {new Date(t.created_at).toLocaleString()}
              </time>
            </div>
            <div className="dash__row-title">{t.description}</div>
            {t.outcome_summary && <div className="dash__row-app">{t.outcome_summary}</div>}
            <div className="dash__row-head">
              <button className="btn" onClick={() => void toggle(t.id)}>
                {open === t.id ? "Hide steps" : "Show steps"}
              </button>
              <button className="btn btn--danger" onClick={() => void purge(t.id)}>
                Purge
              </button>
            </div>
            {open === t.id && (
              <ol className="dash__steps">
                {steps.length === 0 && <li className="dash__empty">No steps recorded.</li>}
                {steps.map((s) => (
                  <li key={`${s.step_number}-${s.timestamp}`}>
                    {s.step_number}. {s.action_type ?? "—"}
                    {s.action_target ? ` "${s.action_target}"` : ""} → {s.result ?? "—"}
                    {s.claude_reasoning ? ` — ${s.claude_reasoning}` : ""}
                    {s.screen_payload_hash && (
                      <>
                        {" "}
                        <code title="SHA-256 of the payload sent for this step">
                          {s.screen_payload_hash.slice(0, 12)}…
                        </code>
                      </>
                    )}
                  </li>
                ))}
              </ol>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}
