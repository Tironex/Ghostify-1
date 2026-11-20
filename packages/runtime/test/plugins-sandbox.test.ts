import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SandboxHost, type Transport } from "../src/plugins/sandbox";
import type { GuestToHost, HostToGuest } from "../src/plugins/protocol";
import { isGuestMessage } from "../src/plugins/protocol";

class FakeTransport implements Transport {
  sent: HostToGuest[] = [];
  destroyed = false;
  listener: (m: unknown) => void = () => undefined;
  send(m: HostToGuest) { this.sent.push(m); }
  onMessage(l: (m: unknown) => void) { this.listener = l; }
  destroy() { this.destroyed = true; }
  fromGuest(m: GuestToHost | Record<string, unknown>) { this.listener(m); }
}

function make(overrides: Partial<ConstructorParameters<typeof SandboxHost>[0]> = {}) {
  const transport = new FakeTransport();
  const onFailure = vi.fn();
  const onLog = vi.fn();
  let now = 0;
  const host = new SandboxHost({
    plugin: { id: "me/p", name: "P", version: "1.0.0", code: "run()" },
    doc: document,
    transportFactory: () => transport,
    handle: async (method, params) => ({ method, params }),
    timeoutMs: 1000,
    onFailure,
    onLog,
    now: () => now,
    ...overrides,
  });
  return { host, transport, onFailure, onLog, setNow: (n: number) => (now = n) };
}

function boot(ctx: ReturnType<typeof make>) {
  ctx.host.start();
  ctx.transport.fromGuest({ t: "ready" });
  ctx.transport.fromGuest({ t: "started" });
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("SandboxHost lifecycle", () => {
  it("sends the code after ready and runs after started", () => {
    const ctx = make();
    ctx.host.start();
    expect(ctx.host.state).toBe("starting");
    ctx.transport.fromGuest({ t: "ready" });
    expect(ctx.transport.sent[0]).toEqual({ t: "init", id: "me/p", name: "P", version: "1.0.0", code: "run()" });
    ctx.transport.fromGuest({ t: "started" });
    expect(ctx.host.state).toBe("running");
    expect(ctx.onFailure).not.toHaveBeenCalled();
  });

  it("can only be started once", () => {
    const ctx = make();
    ctx.host.start();
    expect(() => ctx.host.start()).toThrow();
  });

  it("reports a timeout when the guest never becomes ready or never starts", async () => {
    const a = make();
    a.host.start();
    await vi.advanceTimersByTimeAsync(1001);
    expect(a.onFailure).toHaveBeenCalledWith("timeout", expect.stringContaining("start"));
    expect(a.transport.destroyed).toBe(true);
    expect(a.host.state).toBe("failed");

    const b = make();
    b.host.start();
    b.transport.fromGuest({ t: "ready" });
    await vi.advanceTimersByTimeAsync(1001);
    expect(b.onFailure).toHaveBeenCalledWith("timeout", expect.stringContaining("finish"));
  });

  it("reports a timeout when heartbeats stop", async () => {
    const ctx = make();
    boot(ctx);
    for (let t = 250; t <= 1000; t += 250) {
      ctx.setNow(t);
      ctx.transport.fromGuest({ t: "hb" });
      await vi.advanceTimersByTimeAsync(250);
    }
    expect(ctx.onFailure).not.toHaveBeenCalled();
    ctx.setNow(3000);
    await vi.advanceTimersByTimeAsync(500);
    expect(ctx.onFailure).toHaveBeenCalledWith("timeout", "the plugin stopped responding");
    expect(ctx.host.state).toBe("failed");
  });

  it("reports guest errors as crashes, once", () => {
    const ctx = make();
    boot(ctx);
    ctx.transport.fromGuest({ t: "error", message: "TypeError: x is undefined" });
    ctx.transport.fromGuest({ t: "error", message: "again" });
    expect(ctx.onFailure).toHaveBeenCalledTimes(1);
    expect(ctx.onFailure).toHaveBeenCalledWith("crash", "TypeError: x is undefined");
  });

  it("a factory that throws is a crash", () => {
    const ctx = make({ transportFactory: () => { throw new Error("no window"); } });
    ctx.host.start();
    expect(ctx.onFailure).toHaveBeenCalledWith("crash", expect.stringContaining("no window"));
  });

  it("stop() tells the plugin, tears down and is silent", async () => {
    const ctx = make();
    boot(ctx);
    ctx.host.stop();
    expect(ctx.transport.sent.at(-1)).toEqual({ t: "stop" });
    expect(ctx.transport.destroyed).toBe(true);
    expect(ctx.host.state).toBe("stopped");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(ctx.onFailure).not.toHaveBeenCalled();
    ctx.host.stop();
  });

  it("ignores messages once stopped and malformed messages always", () => {
    const ctx = make();
    boot(ctx);
    for (const junk of [null, 5, "x", {}, { t: "call" }, { t: "call", cid: 1.5, method: "x" }, { t: "log", level: "fatal", text: "x" }, { t: "error" }, { t: "nope" }]) ctx.transport.fromGuest(junk as never);
    expect(ctx.onFailure).not.toHaveBeenCalled();
    ctx.host.stop();
    ctx.transport.fromGuest({ t: "error", message: "late" });
    expect(ctx.onFailure).not.toHaveBeenCalled();
  });
});

describe("SandboxHost calls and events", () => {
  it("answers calls with results and errors", async () => {
    const ctx = make({ handle: async (m) => { if (m === "bad") throw new Error("nope"); return { ok: m }; } });
    boot(ctx);
    ctx.transport.fromGuest({ t: "call", cid: 1, method: "good", params: null });
    ctx.transport.fromGuest({ t: "call", cid: 2, method: "bad", params: null });
    await vi.advanceTimersByTimeAsync(0);
    expect(ctx.transport.sent).toContainEqual({ t: "result", cid: 1, ok: true, value: { ok: "good" } });
    expect(ctx.transport.sent).toContainEqual({ t: "result", cid: 2, ok: false, error: "nope" });
  });

  it("limits calls per second", async () => {
    const ctx = make({ maxCallsPerSecond: 3 });
    boot(ctx);
    for (let cid = 1; cid <= 5; cid++) ctx.transport.fromGuest({ t: "call", cid, method: "x", params: null });
    await vi.advanceTimersByTimeAsync(0);
    const limited = ctx.transport.sent.filter((m) => m.t === "result" && !m.ok);
    expect(limited).toHaveLength(2);
    ctx.setNow(1500);
    ctx.transport.fromGuest({ t: "call", cid: 6, method: "x", params: null });
    await vi.advanceTimersByTimeAsync(0);
    expect(ctx.transport.sent.at(-1)).toMatchObject({ cid: 6, ok: true });
  });

  it("forwards events only to a running plugin and logs", () => {
    const ctx = make();
    ctx.host.emit("player.change", 1);
    expect(ctx.transport.sent).toEqual([]);
    boot(ctx);
    ctx.host.emit("player.change", { playing: true });
    expect(ctx.transport.sent.at(-1)).toEqual({ t: "event", name: "player.change", payload: { playing: true } });
    ctx.transport.fromGuest({ t: "log", level: "warn", text: "careful" });
    expect(ctx.onLog).toHaveBeenCalledWith("warn", "careful");
  });

  it("truncates long error messages", async () => {
    const ctx = make({ handle: async () => { throw new Error("e".repeat(2000)); } });
    boot(ctx);
    ctx.transport.fromGuest({ t: "call", cid: 1, method: "x", params: null });
    await vi.advanceTimersByTimeAsync(0);
    const reply = ctx.transport.sent.at(-1) as { error: string };
    expect(reply.error).toHaveLength(500);
  });
});

describe("isGuestMessage", () => {
  it("accepts the documented shapes", () => {
    for (const m of [{ t: "ready" }, { t: "started" }, { t: "hb" }, { t: "call", cid: 1, method: "a.b", params: 1 }, { t: "error", message: "x" }, { t: "log", level: "info", text: "x" }]) {
      expect(isGuestMessage(m)).toBe(true);
    }
    expect(isGuestMessage({ t: "call", cid: 1, method: "x".repeat(65) })).toBe(false);
  });
});
