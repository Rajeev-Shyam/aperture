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
  setAutostart,
  type DashboardStats,
  type HistoryEvent,
  type PatternRow,
  type SuggestionHistoryRow,
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

function VoiceTab() {
  const [stats, setStats] = useState<DashboardStats | null>(null);
  const [chord, setChord] = useState<string>("Ctrl+Alt+Space");
  const [error, setError] = useState<string | null>(null);

  function refresh() {
    void dashboardStats().then(setStats).catch((e) => setError(String(e)));
    void getSettings()
      .then((s) => {
        const v = s.voice as { ptt_hotkey?: string } | undefined;
        if (v?.ptt_hotkey) setChord(v.ptt_hotkey);
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
                {s.useful_rating ? ` · ${s.useful_rating === "up" ? "👍" : "👎"}` : ""}
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
