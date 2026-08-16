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
  vlmDownload,
  vlmStatus,
  type DashboardStats,
  type HistoryEvent,
  type PatternRow,
  type SuggestionHistoryRow,
  type VlmFetchEvent,
  type VlmStatus,
  type VoiceSettings,
} from "../lib/ipc";
import { useDraggable } from "../state/useDraggable";
import { useModalSurface } from "../state/useModalSurface";

type Tab = "overview" | "history" | "patterns" | "suggestions" | "voice";

const TABS: { id: Tab; label: string; hint: string }[] = [
  { id: "overview", label: "Overview", hint: "What Aperture holds right now" },
  { id: "history", label: "History", hint: "Everything captured, searchable" },
  { id: "patterns", label: "Patterns", hint: "Habits the engine has mined" },
  { id: "suggestions", label: "Suggestions", hint: "Every bubble ever surfaced" },
  { id: "voice", label: "Voice", hint: "Push-to-talk transcripts" },
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

  useEffect(refresh, []);

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

  /** Persist the confirm floor (decision #27): merge into the voice section so
   *  the write can't drop sibling keys. The core reads it per utterance — the
   *  change applies to the very next press, no restart. */
  function updateFloor(v: number) {
    setFloor(v);
    voiceSection.current = { ...voiceSection.current, intent_confidence_floor: v };
    void setSettings({ voice: voiceSection.current }).catch((e) => setError(String(e)));
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
