import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PluginManager, type EnabledPlugin } from "../src/plugins/manager";
import type { Transport } from "../src/plugins/sandbox";
import type { GuestToHost, HostToGuest } from "../src/plugins/protocol";

class FakeTransport implements Transport {
  sent: HostToGuest[] = [];
  destroyed = false;
  listener: (m: unknown) => void = () => undefined;
  send(m: HostToGuest) { this.sent.push(m); }
  onMessage(l: (m: unknown) => void) { this.listener = l; }
  destroy() { this.destroyed = true; }
  fromGuest(m: GuestToHost) { this.listener(m); }
}

function enabled(id: string, extra: Partial<EnabledPlugin> = {}): EnabledPlugin {
  return { id, name: id, version: "1.0.0", runtime: "sandboxed", permissions: [], hosts: [], ...extra };
}

function setup(initial: EnabledPlugin[], options: Record<string, unknown> = {}) {
  let list = initial;
  const transports: FakeTransport[] = [];
  const reported: any[] = [];
  const handlers = new Map<string, (p: any) => unknown>();
  const listeners = new Map<string, Array<(p: unknown) => void>>();
  let outcome: any = { crashes_in_window: 1, disabled: false, rolled_back_to: null };
  const stateHandlers: Array<(s: string) => void> = [];
  const client = {
    connectionState: "open" as const,
    onState: (h: (s: string) => void) => { stateHandlers.push(h); return () => undefined; },
    on: (m: string, h: (p: unknown) => void) => { listeners.set(m, [...(listeners.get(m) ?? []), h]); return () => undefined; },
    call: vi.fn(async (method: string, params?: any) => {
      handlers.get(method)?.(params);
      switch (method) {
        case "plugin.enabled": return list;
        case "plugin.load": {
          const found = list.find((p) => p.id === params.id);
          if (!found) throw new Error("not enabled");
          return { ...found, sha256: "h", code: `code-of-${found.id}` };
        }
        case "plugin.report_crash": reported.push(params); return outcome;
        default: return null;
      }
    }),
  };
  const manager = new PluginManager({
    client: client as never,
    doc: document,
    adapters: { ui: { notify: vi.fn() } },
    version: "0.1.0",
    transportFactory: () => { const t = new FakeTransport(); transports.push(t); return t; },
    hookTimeoutMs: 1000,
    minRestartMs: 100,
    maxRestartMs: 1000,
    ...options,
  });
  return {
    manager, client, transports, reported,
    setList: (l: EnabledPlugin[]) => (list = l),
    setOutcome: (o: unknown) => (outcome = o),
    notify: (m: string) => listeners.get(m)?.forEach((h) => h(null)),
    reconnect: () => stateHandlers.forEach((h) => h("open")),
  };
}

const flush = () => vi.advanceTimersByTimeAsync(0);
const bootAll = (t: FakeTransport) => { t.fromGuest({ t: "ready" }); t.fromGuest({ t: "started" }); };

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("PluginManager sync", () => {
  it("starts enabled sandboxed plugins with their code", async () => {
    const s = setup([enabled("a/one"), enabled("b/two")]);
    s.manager.start();
    await flush();
    expect(s.manager.runningIds).toEqual(["a/one", "b/two"]);
    s.transports[0]!.fromGuest({ t: "ready" });
    expect(s.transports[0]!.sent[0]).toMatchObject({ t: "init", id: "a/one", code: "code-of-a/one" });
    expect(s.client.call).toHaveBeenCalledWith("plugin.report_state", { id: "a/one", state: "active" });
  });

  it("stops plugins that were disabled and starts new ones on plugins.changed", async () => {
    const s = setup([enabled("a/one")]);
    s.manager.start();
    await flush();
    const first = s.transports[0]!;
    s.setList([enabled("b/two")]);
    s.notify("plugins.changed");
    await flush();
    expect(s.manager.runningIds).toEqual(["b/two"]);
    expect(first.destroyed).toBe(true);
  });

  it("restarts a plugin whose version or permissions changed, keeps unchanged ones", async () => {
    const s = setup([enabled("a/one"), enabled("b/two")]);
    s.manager.start();
    await flush();
    const [a, b] = s.transports as [FakeTransport, FakeTransport];
    s.setList([enabled("a/one", { version: "2.0.0" }), enabled("b/two")]);
    await s.manager.sync();
    expect(a.destroyed).toBe(true);
    expect(b.destroyed).toBe(false);
    expect(s.transports).toHaveLength(3);
  });

  it("syncs again after a reconnect and tolerates a failing list call", async () => {
    const s = setup([enabled("a/one")]);
    s.manager.start();
    await flush();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    s.client.call.mockImplementationOnce(async () => { throw new Error("bridge down"); });
    await s.manager.sync();
    expect(s.manager.runningIds).toEqual(["a/one"]);
    s.reconnect();
    await flush();
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });

  it("skips a plugin the bridge refuses to load", async () => {
    const s = setup([enabled("a/one")]);
    s.client.call.mockImplementation(async (method: string) => {
      if (method === "plugin.enabled") return [enabled("a/one")];
      throw new Error("files were modified");
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    s.manager.start();
    await flush();
    expect(s.manager.runningIds).toEqual([]);
    warn.mockRestore();
  });

  it("stopAll stops everything", async () => {
    const s = setup([enabled("a/one")]);
    s.manager.start();
    await flush();
    s.manager.stopAll();
    expect(s.manager.runningIds).toEqual([]);
    expect(s.transports[0]!.destroyed).toBe(true);
  });
});

describe("PluginManager crash handling", () => {
  it("reports crashes and restarts with growing delays", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const s = setup([enabled("a/one")], { localCrashLimit: 10 });
    s.manager.start();
    await flush();
    bootAll(s.transports[0]!);
    s.transports[0]!.fromGuest({ t: "error", message: "boom" });
    await flush();
    expect(s.reported[0]).toEqual({ id: "a/one", kind: "crash", detail: "boom" });
    expect(s.transports).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(100);
    expect(s.transports).toHaveLength(2);

    bootAll(s.transports[1]!);
    s.transports[1]!.fromGuest({ t: "error", message: "boom2" });
    await flush();
    await vi.advanceTimersByTimeAsync(150);
    expect(s.transports).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(60);
    expect(s.transports).toHaveLength(3);
    error.mockRestore();
  });

  it("does not restart when the policy disabled the plugin, and resyncs", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const s = setup([enabled("a/one")]);
    s.manager.start();
    await flush();
    bootAll(s.transports[0]!);
    s.setOutcome({ crashes_in_window: 3, disabled: true, rolled_back_to: "0.9.0" });
    s.setList([]);
    s.transports[0]!.fromGuest({ t: "error", message: "boom" });
    await flush();
    await flush();
    await vi.advanceTimersByTimeAsync(5000);
    expect(s.manager.runningIds).toEqual([]);
    expect(s.transports).toHaveLength(1);
    error.mockRestore();
  });

  it("stops locally after repeated crashes when the app cannot be reached", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const s = setup([enabled("a/one")], { localCrashLimit: 2 });
    s.client.call.mockImplementation(async (method: string, params?: any) => {
      if (method === "plugin.enabled") return [enabled("a/one")];
      if (method === "plugin.load") return { ...enabled("a/one"), sha256: "h", code: "c" };
      if (method === "plugin.report_crash") throw new Error("offline");
      void params;
      return null;
    });
    s.manager.start();
    await flush();
    for (let i = 0; i < 2; i++) {
      const t = s.transports.at(-1)!;
      bootAll(t);
      t.fromGuest({ t: "error", message: "boom" });
      await flush();
      await vi.advanceTimersByTimeAsync(2000);
    }
    expect(s.manager.runningIds).toEqual([]);
    expect(s.transports).toHaveLength(2);
    error.mockRestore();
    warn.mockRestore();
  });

  it("a heartbeat timeout is reported as a timeout", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const s = setup([enabled("a/one")]);
    s.manager.start();
    await flush();
    s.transports[0]!.fromGuest({ t: "ready" });
    await vi.advanceTimersByTimeAsync(1100);
    expect(s.reported[0]).toMatchObject({ id: "a/one", kind: "timeout" });
    error.mockRestore();
  });
});
