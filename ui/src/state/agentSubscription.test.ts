// 08-22 review [low]: the `agent_task` subscription must not leak on a fast
// unmount / StrictMode remount, and a late `agent_status` snapshot must not
// overwrite a newer streamed view.

import { describe, expect, it } from "vitest";

import { subscribeAgentTask, type AgentTaskIo } from "./agentSubscription";

type View = Parameters<Parameters<typeof subscribeAgentTask>[0]>[0];
const view = (id: string) => ({ task_id: id }) as unknown as View;

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}
const tick = () => new Promise<void>((r) => setTimeout(r, 0));

describe("subscribeAgentTask", () => {
  it("delivers the snapshot, then the stream, and unlistens on cleanup", async () => {
    const seen: View[] = [];
    let handler: ((v: View) => void) | undefined;
    let unlistened = 0;
    const io: AgentTaskIo = {
      status: () => Promise.resolve(view("a")),
      listen: async (h) => {
        handler = h;
        return () => {
          unlistened += 1;
        };
      },
    };
    const cleanup = subscribeAgentTask((v) => seen.push(v), io);
    await tick();
    expect(seen).toEqual([view("a")]);
    handler?.(view("b"));
    expect(seen).toEqual([view("a"), view("b")]);
    cleanup();
    expect(unlistened).toBe(1);
    handler?.(view("c"));
    expect(seen).toHaveLength(2); // after cleanup a straggling event is dropped
  });

  it("releases a listener that resolves after cleanup (StrictMode remount)", async () => {
    const unlisten = deferred<() => void>();
    let unlistened = 0;
    const io: AgentTaskIo = {
      status: () => new Promise(() => {}),
      listen: () => unlisten.promise,
    };
    const cleanup = subscribeAgentTask(() => {}, io);
    cleanup();
    unlisten.resolve(() => {
      unlistened += 1;
    });
    await tick();
    expect(unlistened).toBe(1);
  });

  it("drops a snapshot that lands after the stream delivered a newer view", async () => {
    const seen: View[] = [];
    const status = deferred<View>();
    let handler: ((v: View) => void) | undefined;
    const io: AgentTaskIo = {
      status: () => status.promise,
      listen: async (h) => {
        handler = h;
        return () => {};
      },
    };
    subscribeAgentTask((v) => seen.push(v), io);
    await tick();
    handler?.(view("new"));
    status.resolve(view("stale"));
    await tick();
    expect(seen).toEqual([view("new")]);
  });

  it("drops a snapshot that lands after cleanup", async () => {
    const seen: View[] = [];
    const status = deferred<View>();
    const io: AgentTaskIo = {
      status: () => status.promise,
      listen: async () => () => {},
    };
    const cleanup = subscribeAgentTask((v) => seen.push(v), io);
    cleanup();
    status.resolve(view("late"));
    await tick();
    expect(seen).toEqual([]);
  });
});
