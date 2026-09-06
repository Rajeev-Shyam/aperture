// Agent surface (Doc 22 §9, owner decision #53): a persistent status bar with
// a live step log while a task exists, plus the cards the loop pauses on —
// task approval (#48), the consequential-action chip (#47/#51), Claude's
// clarification question, an excluded/elevated-window pause (#49/#50) — and
// the terminal card with the honest undo offer (#54).
//
// STOP is always visible while the task is live (locked decision 5). It is
// wired to the same `agent_decide(stop)` path the tray item uses: the core
// flags the executor BEFORE anything else, so an action that has not started
// yet is refused even if this WebView is slow to repaint.
//
// Wears `surface-opaque` (doc 14 §5 glass budget — a long-lived surface must
// not spend a glass slot) and `surface-interactive` (hit-test registration is
// implicit, see useHitTestRects).

import { useEffect, useRef, useState, type ReactNode } from "react";

import {
  agentAnswer,
  agentDecide,
  agentDismiss,
  agentStartTask,
  agentStatus,
  agentUndoCloseWindows,
  onAgentTask,
  type AgentAction,
  type AgentDecision,
  type AgentTaskView,
} from "../lib/ipc";
import { subscribeAgentTask } from "../state/agentSubscription";
import { useModalSurface } from "../state/useModalSurface";

/** Plain-English rendering of an action Claude wants to take. */
export function describeAction(a: AgentAction): string {
  const t = a.target ? `"${a.target}"` : "";
  switch (a.type) {
    case "click":
      return `click ${t}`.trim();
    case "type":
      return `type ${a.value ? `"${a.value}"` : "text"}${a.target ? ` into ${t}` : ""}`;
    case "key":
      return `press ${a.value ?? "a key"}`;
    case "launch":
      return `open ${t || "an app"} via the Start menu`;
    case "switch_window":
      return `switch to ${t || "a window"}`;
    case "scroll":
      return `scroll ${a.direction ?? "down"}${a.amount ? ` ×${a.amount}` : ""}`;
    case "wait":
      return `wait ${a.amount ?? 1}s`;
    default:
      return "do nothing";
  }
}

const TERMINAL = new Set(["complete", "failed", "cancelled"]);

interface Props {
  /** The "New task" input is open (HUD button) — rendered even with no task. */
  composing: boolean;
  onCloseComposer: () => void;
}

export function AgentSurface({ composing, onCloseComposer }: Props) {
  const [view, setView] = useState<AgentTaskView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Snapshot + stream, with the unmount races closed (08-22 review; the
  // logic and its tests live in state/agentSubscription.ts).
  useEffect(() => subscribeAgentTask(setView, { status: agentStatus, listen: onAgentTask }), []);

  async function decide(decision: AgentDecision) {
    if (!view) return;
    setBusy(true);
    setError(null);
    try {
      setView(await agentDecide(view.task_id, decision));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  if (!view && !composing) return null;

  return (
    <div className="agent" role="region" aria-label="Agent task">
      {composing && (
        <TaskComposer
          disabled={!!view && !TERMINAL.has(view.state)}
          onClose={onCloseComposer}
          onStarted={(v) => {
            setView(v);
            onCloseComposer();
          }}
        />
      )}
      {view && (
        <div className="agent__bar surface-opaque surface-interactive">
          <div className="agent__head">
            {/* A stop pressed mid-action is acknowledged immediately (the
                core applies it when the action returns) — never a stale
                "running"/"acting". */}
            <span className="agent__state" data-state={view.state}>
              {view.stopping ? "stopping…" : view.in_flight ? "acting" : view.state}
            </span>
            <span className="agent__title" title={view.description}>
              {view.description}
            </span>
            <span className="agent__step">
              step {view.step}/{view.step_cap}
            </span>
            {!TERMINAL.has(view.state) ? (
              <button
                className="btn agent__stop"
                onClick={() => void decide("stop")}
                title="Stop now — no further actions"
              >
                ■ Stop
              </button>
            ) : (
              <button
                className="btn btn--icon"
                aria-label="Dismiss"
                onClick={() => void agentDismiss().then(() => setView(null)).catch((e) => setError(String(e)))}
              >
                ×
              </button>
            )}
          </div>

          {view.pause && !TERMINAL.has(view.state) && (
            <PauseCard view={view} busy={busy} onDecide={decide} onAnswered={setView} onError={setError} />
          )}

          {TERMINAL.has(view.state) && (
            <div className="agent__card" role="status">
              <p className="agent__card-lead">{view.stop_reason ?? view.state}</p>
              {view.outcome && <p className="agent__muted">{view.outcome}</p>}
              {view.undoable_windows.length > 0 && (
                <div className="agent__actions">
                  <button
                    className="btn"
                    disabled={busy}
                    onClick={() => {
                      setBusy(true);
                      void agentUndoCloseWindows(view.task_id)
                        .then((n) => setError(n > 0 ? null : "nothing left to close"))
                        .catch((e) => setError(String(e)))
                        .finally(() => setBusy(false));
                    }}
                    title={view.undoable_windows.map((w) => w.title).join(", ")}
                  >
                    Close {view.undoable_windows.length} window{view.undoable_windows.length === 1 ? "" : "s"} it opened
                  </button>
                  <span className="agent__muted">
                    Typed text and clicks can't be undone by Aperture.
                  </span>
                </div>
              )}
            </div>
          )}

          <ol className="agent__log" aria-label="Step log">
            {view.log.length === 0 && <li className="agent__muted">No actions yet.</li>}
            {view.log.map((e) => (
              <li key={`${e.step_number}-${e.ts}`} className="agent__log-row" data-result={e.result}>
                <span className="agent__log-n">{e.step_number}</span>
                <span className="agent__log-summary">{e.summary}</span>
                <span className="agent__log-result">{e.result}</span>
                <span className="agent__log-rev" title={`reversibility: ${e.reversibility}`}>
                  {e.reversibility === "reversible" ? "↺" : e.reversibility === "irreversible" ? "⚠" : "·"}
                </span>
              </li>
            ))}
          </ol>
          {error && (
            <p className="agent__error" role="alert">
              {error}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

function PauseCard({
  view,
  busy,
  onDecide,
  onAnswered,
  onError,
}: {
  view: AgentTaskView;
  busy: boolean;
  onDecide: (d: AgentDecision) => Promise<void>;
  onAnswered: (v: AgentTaskView) => void;
  onError: (e: string) => void;
}) {
  const [answer, setAnswer] = useState("");
  const p = view.pause!;
  switch (p.kind) {
    case "approval":
      return (
        <div className="agent__card" role="alertdialog" aria-label="Approve agent task">
          <p className="agent__card-lead">
            {view.source === "claude" ? "Claude wants to run a task on this PC:" : "Start this task?"}
          </p>
          <p className="agent__quote">{view.description}</p>
          <p className="agent__muted">
            While it runs, every step sends Claude a redacted screenshot, the redacted on-screen
            text, window titles, and the task. Secrets, card numbers, emails and phones are
            blanked first. It never runs on excluded apps. You can stop at any moment.
          </p>
          <div className="agent__actions">
            <button className="btn btn--primary" disabled={busy} onClick={() => void onDecide("approve")}>
              Allow this task
            </button>
            <button className="btn" disabled={busy} onClick={() => void onDecide("deny")}>
              Deny
            </button>
          </div>
        </div>
      );
    case "confirm":
      return (
        <ModalCard key={p.kind} label="Confirm action">
          <p className="agent__card-lead">Claude wants to {describeAction(p.action)}</p>
          <p className="agent__muted">Paused because it {p.reason}.</p>
          <div className="agent__actions">
            <button className="btn btn--primary" disabled={busy} onClick={() => void onDecide("confirm")}>
              Approve
            </button>
            <button className="btn" disabled={busy} onClick={() => void onDecide("skip")}>
              Skip this step
            </button>
            <button className="btn" onClick={() => void onDecide("stop")}>
              Stop
            </button>
          </div>
        </ModalCard>
      );
    case "clarification":
      return (
        <ModalCard key={p.kind} label="Claude has a question">
          <p className="agent__card-lead">Claude asks:</p>
          <p className="agent__quote">{p.question}</p>
          <form
            className="agent__actions"
            onSubmit={(e) => {
              e.preventDefault();
              if (!answer.trim()) return;
              void agentAnswer(view.task_id, answer.trim())
                .then((v) => {
                  onAnswered(v);
                  setAnswer("");
                })
                .catch((err) => onError(String(err)));
            }}
          >
            <input
              className="agent__input"
              value={answer}
              onChange={(e) => setAnswer(e.target.value)}
              placeholder="Your answer"
              aria-label="Answer"
              autoFocus
            />
            <button className="btn btn--primary" type="submit" disabled={busy || !answer.trim()}>
              Send
            </button>
            <button className="btn" type="button" onClick={() => void onDecide("stop")}>
              Stop
            </button>
          </form>
        </ModalCard>
      );
    case "excluded":
      return (
        <div className="agent__card" role="alertdialog" aria-label="Excluded app in front">
          <p className="agent__card-lead">Paused — "{p.label}" is on your exclusion list.</p>
          <p className="agent__muted">
            Aperture will not look at or act on it. Switch to another window, then resume.
          </p>
          <div className="agent__actions">
            <button className="btn btn--primary" disabled={busy} onClick={() => void onDecide("resume")}>
              Resume
            </button>
            <button className="btn" onClick={() => void onDecide("stop")}>
              Stop
            </button>
          </div>
        </div>
      );
    case "elevated":
      return (
        <div className="agent__card" role="alertdialog" aria-label="Administrator window in front">
          <p className="agent__card-lead">Paused — "{p.window}" runs as administrator.</p>
          <p className="agent__muted">
            Windows blocks Aperture's input to elevated windows (it runs as you, unelevated).
            Finish that step yourself or switch to another window, then resume.
          </p>
          <div className="agent__actions">
            <button className="btn btn--primary" disabled={busy} onClick={() => void onDecide("resume")}>
              Resume
            </button>
            <button className="btn" onClick={() => void onDecide("stop")}>
              Stop
            </button>
          </div>
        </div>
      );
    case "vram":
      return (
        <div className="agent__card" role="status">
          <p className="agent__card-lead">Paused — GPU memory pressure.</p>
          <div className="agent__actions">
            <button className="btn btn--primary" disabled={busy} onClick={() => void onDecide("resume")}>
              Resume
            </button>
            <button className="btn" onClick={() => void onDecide("stop")}>
              Stop
            </button>
          </div>
        </div>
      );
    default:
      return null;
  }
}

/** A pause card the user must ANSWER from the keyboard (Claude's question, the
 *  consequential-action chip). The overlay window is created `focus:false`, so
 *  a bare `<input autoFocus>` in the hover-only bar moved DOM focus while OS
 *  keystrokes kept going to the app Claude was just driving (08-22 review):
 *  `useModalSurface` asks for OS focus, moves DOM focus in unless a child's
 *  `autoFocus` already did, cycles Tab inside, and restores focus on close.
 *  Callers key it by pause kind so a kind switch is a fresh surface.
 *  The other cards (approval, excluded, elevated, vram) stay click-only: they
 *  appear while the user may be working in another app and must not take its
 *  focus. */
function ModalCard({ label, children }: { label: string; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const onKeyDown = useModalSurface(ref, { exclusive: false });
  return (
    <div
      ref={ref}
      className="agent__card"
      role="alertdialog"
      aria-label={label}
      tabIndex={-1}
      onKeyDown={onKeyDown}
    >
      {children}
    </div>
  );
}

/** Doc 22 §9.1 — the user types a task; Claude Desktop then adopts it. */
function TaskComposer({
  disabled,
  onClose,
  onStarted,
}: {
  disabled: boolean;
  onClose: () => void;
  onStarted: (v: AgentTaskView) => void;
}) {
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const onKeyDown = useModalSurface(ref, { exclusive: false });

  return (
    <div
      ref={ref}
      className="agent__composer surface-opaque surface-interactive"
      role="dialog"
      aria-label="New agent task"
      tabIndex={-1}
      onKeyDown={(e) => {
        if (e.key === "Escape") onClose();
        else onKeyDown(e);
      }}
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!text.trim() || disabled) return;
          void agentStartTask(text.trim())
            .then(onStarted)
            .catch((err) => setError(String(err)));
        }}
      >
        <label className="agent__muted" htmlFor="agent-task-text">
          What should the agent do? (then tell Claude Desktop: "run my Aperture task")
        </label>
        <textarea
          id="agent-task-text"
          className="agent__input agent__textarea"
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder="e.g. Open Notepad and write today's date"
          rows={2}
          autoFocus
          disabled={disabled}
        />
        {disabled && <p className="agent__muted">A task is already running — stop it first.</p>}
        {error && (
          <p className="agent__error" role="alert">
            {error}
          </p>
        )}
        <div className="agent__actions">
          <button className="btn btn--primary" type="submit" disabled={disabled || !text.trim()}>
            Start task
          </button>
          <button className="btn" type="button" onClick={onClose}>
            Cancel
          </button>
        </div>
      </form>
    </div>
  );
}
