// Wire an agent surface to the core: an initial `agent_status` snapshot plus
// the live `agent_task` stream — with the cleanup races closed (08-22 review).
//
// Pure with respect to Tauri: the two IPC calls are injected, so the logic is
// unit-tested (agentSubscription.test.ts) without a WebView.

import type { AgentTaskView } from "../lib/ipc";

export interface AgentTaskIo {
  /** `agent_status` — the current snapshot, or null when no task exists. */
  status: () => Promise<AgentTaskView | null>;
  /** Subscribe to `agent_task`; resolves to the unlisten function. */
  listen: (h: (view: AgentTaskView | null) => void) => Promise<() => void>;
}

/**
 * Both halves are promises, so the effect's cleanup can run before either
 * settles (a fast unmount; React.StrictMode's mount→unmount→mount in dev):
 *
 * - a listener that resolves AFTER cleanup is released on the spot instead of
 *   leaking a handler that calls `setView` on an unmounted component
 *   (App.tsx's pattern);
 * - a snapshot that lands late — after cleanup, or after the stream already
 *   delivered a newer view — is dropped rather than overwriting it.
 *
 * Returns the effect's cleanup.
 */
export function subscribeAgentTask(
  setView: (v: AgentTaskView | null) => void,
  io: AgentTaskIo,
): () => void {
  let cancelled = false;
  let sawEvent = false;
  let unlisten: (() => void) | undefined;
  void io
    .status()
    .then((v) => {
      if (!cancelled && !sawEvent) setView(v);
    })
    .catch(() => {});
  void io
    .listen((v) => {
      if (cancelled) return;
      sawEvent = true;
      setView(v);
    })
    .then((u) => {
      if (cancelled) u();
      else unlisten = u;
    })
    .catch(() => {});
  return () => {
    cancelled = true;
    unlisten?.();
  };
}
