//! PrivacyPanel — the Activity & Privacy view (doc 13 §3, §7; ADR-040).
//
//  Answers the two questions the product promises the user can always answer:
//    · "When was it watching?"        → `capture_toggle` audit rows
//    · "What ever left this machine?" → `cloud_send` rows, each with the SHA-256
//      of the exact bytes, the transport, and the byte count.
//
//  Plus the two controls that make the promises actionable: the exclusion list
//  (add / disable / delete) and Purge All.
//
//  Purge All is irreversible, so it uses a typed confirmation rather than a
//  one-click button — and it never triggers a browser `confirm()`, which would
//  block the WebView. The panel states plainly what survives a purge, because a
//  privacy control that quietly keeps data would be the worst possible surprise.
//
//  Opaque chrome, not glass: a full-height side panel would blow the ≤2 glass
//  budget (doc 14 §5) on the largest surface in the app.

import { useCallback, useEffect, useRef, useState } from "react";

import {
  addExclusion,
  listAudit,
  listExclusions,
  purgeAll,
  setExclusion,
  type AuditRow,
  type ExclusionKind,
  type ExclusionRow,
} from "../lib/ipc";
import { useModalSurface } from "../state/useModalSurface";

interface Props {
  /** Whether at-rest encryption is actually in force, per the core. */
  dbEncrypted: boolean;
  onClose: () => void;
}

const KIND_LABELS: Record<ExclusionKind, string> = {
  process: "App (process)",
  window_class: "Window class",
  title_regex: "Window title (regex)",
  url_pattern: "URL (regex)",
};

const PURGE_PHRASE = "DELETE";

export function PrivacyPanel({ dbEncrypted, onClose }: Props) {
  const [audit, setAudit] = useState<AuditRow[]>([]);
  const [rules, setRules] = useState<ExclusionRow[]>([]);
  const [newKind, setNewKind] = useState<ExclusionKind>("process");
  const [newPattern, setNewPattern] = useState("");
  const [purgeInput, setPurgeInput] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  // Backs the `aria-modal` declaration below with the real contract.
  const trapKeys = useModalSurface(rootRef);

  const refresh = useCallback(async () => {
    try {
      const [a, r] = await Promise.all([listAudit(200), listExclusions()]);
      setAudit(a);
      setRules(r);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function onAdd() {
    if (!newPattern.trim()) return;
    setError(null);
    try {
      await addExclusion(newKind, newPattern.trim());
      setNewPattern("");
      await refresh();
      setStatus("Rule added. It applies to captures from the next launch.");
    } catch (e) {
      setError(String(e));
    }
  }

  async function onSetRule(id: number, enabled: boolean | null) {
    setError(null);
    try {
      await setExclusion(id, enabled);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  async function onPurge() {
    if (purgeInput !== PURGE_PHRASE) return;
    setError(null);
    try {
      const deleted = await purgeAll();
      setPurgeInput("");
      await refresh();
      setStatus(`Purged ${deleted} history ${deleted === 1 ? "row" : "rows"}.`);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div
      className="privacy surface-opaque surface-interactive"
      role="dialog"
      aria-modal="true"
      aria-label="Activity and privacy"
      ref={rootRef}
      tabIndex={-1}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          onClose(); // safe exit: this panel only reads + confirms, never sends
          return;
        }
        trapKeys(e);
      }}
    >
      <header className="privacy__head">
        <h2>Activity &amp; Privacy</h2>
        <button className="btn btn--icon" aria-label="Close" onClick={onClose}>
          ×
        </button>
      </header>

      <p className={`privacy__encryption ${dbEncrypted ? "" : "privacy__encryption--warn"}`}>
        {dbEncrypted
          ? "History is encrypted at rest, keyed to your Windows account."
          : "This build does NOT encrypt history at rest — it is plaintext on disk."}
      </p>

      {/* --- What left this machine / when was it watching (doc 13 §3) ------- */}
      <section className="privacy__section" aria-label="Audit log">
        <h3>Activity log</h3>
        {audit.length === 0 && <p className="privacy__note">Nothing recorded yet.</p>}
        <ul className="privacy__audit">
          {audit.map((row) => (
            <li key={row.id} className={`privacy__audit-row privacy__audit-row--${row.type}`}>
              <time dateTime={new Date(row.ts).toISOString()}>
                {new Date(row.ts).toLocaleString()}
              </time>
              {row.type === "capture_toggle" ? (
                <span>
                  {/* `source` separates the decision row (the user chose) from
                      the mechanism row (capture actually started). Both are on
                      the trail; conflating them would hide a capture that was
                      requested but never ran. */}
                  {row.payload.source === "capture" ? "Capture " : "Capture set to "}
                  <strong>{row.payload.enabled ? "on" : "off"}</strong>
                  {row.payload.source === "capture" && " (took effect)"}
                  {row.payload.source !== "capture" &&
                    typeof row.payload.reason === "string" &&
                    ` (${row.payload.reason})`}
                </span>
              ) : (
                <span>
                  Sent <strong>{String(row.payload.byte_count)} bytes</strong> via{" "}
                  {String(row.payload.transport)}
                  <code className="privacy__hash" title="SHA-256 of the exact bytes sent">
                    {String(row.payload.wire_sha256).slice(0, 16)}…
                  </code>
                </span>
              )}
            </li>
          ))}
        </ul>
      </section>

      {/* --- Exclusions (doc 13 §4) ----------------------------------------- */}
      <section className="privacy__section" aria-label="Excluded apps and sites">
        <h3>Never capture these</h3>
        {rules.length === 0 && (
          <p className="privacy__note">
            No exclusions. Nothing is excluded by default — that is your choice to make.
          </p>
        )}
        <ul className="privacy__rules">
          {rules.map((r) => (
            <li key={r.id} className={r.enabled ? "" : "privacy__rule--off"}>
              <span className="privacy__rule-kind">{KIND_LABELS[r.match_kind]}</span>
              <code>{r.pattern}</code>
              <button className="btn" onClick={() => void onSetRule(r.id, !r.enabled)}>
                {r.enabled ? "Disable" : "Enable"}
              </button>
              <button
                className="btn btn--icon"
                aria-label={`Delete rule ${r.pattern}`}
                onClick={() => void onSetRule(r.id, null)}
              >
                ×
              </button>
            </li>
          ))}
        </ul>
        <div className="privacy__add">
          <select
            value={newKind}
            aria-label="Match kind"
            onChange={(e) => setNewKind(e.target.value as ExclusionKind)}
          >
            {(Object.keys(KIND_LABELS) as ExclusionKind[]).map((k) => (
              <option key={k} value={k}>
                {KIND_LABELS[k]}
              </option>
            ))}
          </select>
          <input
            value={newPattern}
            aria-label="Pattern"
            placeholder={newKind === "process" ? "1password.exe" : "^https://banking\\."}
            onChange={(e) => setNewPattern(e.target.value)}
          />
          <button className="btn" onClick={() => void onAdd()} disabled={!newPattern.trim()}>
            Add
          </button>
        </div>
      </section>

      {/* --- Purge All (doc 13 §7) ------------------------------------------ */}
      <section className="privacy__section privacy__danger" aria-label="Purge all data">
        <h3>Purge everything</h3>
        <p className="privacy__note">
          Deletes all captured history, patterns, and suggestions, then reclaims the
          disk space. This cannot be undone.
        </p>
        <p className="privacy__note">
          Kept on purpose: your exclusion rules and consent settings (so your
          protections do not quietly reset), and the last 30 days of this activity
          log (so you can still audit what left the machine).
        </p>
        <div className="privacy__add">
          <input
            value={purgeInput}
            aria-label={`Type ${PURGE_PHRASE} to confirm`}
            placeholder={`Type ${PURGE_PHRASE} to confirm`}
            onChange={(e) => setPurgeInput(e.target.value)}
          />
          <button
            className="btn btn--danger"
            onClick={() => void onPurge()}
            disabled={purgeInput !== PURGE_PHRASE}
          >
            Purge all
          </button>
        </div>
      </section>

      {status && (
        <p className="privacy__status" role="status">
          {status}
        </p>
      )}
      {error && (
        <p className="privacy__error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
