/** The real guest script inside a real iframe: what a plugin actually sees. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createCapabilityHandler } from "../src/plugins/capabilities";
import { JSDOM } from "jsdom";
import { SandboxHost, iframeTransport, type Transport, type TransportFactory } from "../src/plugins/sandbox";
import type { HostToGuest } from "../src/plugins/protocol";

/**
 * jsdom cannot load iframe[srcdoc], so the guest runs in a separate jsdom window
 * with scripts enabled. There `window.parent` is the window itself; messages the
 * guest posts are captured, and host messages are dispatched as message events
 * whose source is that window. CSP and the sandbox attribute are browser
 * features and are not enforced here.
 */
const jsdomTransport: TransportFactory = () => {
  let listener: (message: unknown) => void = () => undefined;
  let dom: JSDOM | null = new JSDOM("<!doctype html><html><head></head><body></body></html>", {
    runScripts: "dangerously",
  });
  const window = dom.window as unknown as Window & typeof globalThis;
  (window as unknown as { postMessage: (m: unknown) => void }).postMessage = (message) => queueMicrotask(() => listener(message));
  // Run the guest the way the iframe's inline script would.
  dom.window.eval(__GUEST_SOURCE__);
  const transport: Transport = {
    send: (message: HostToGuest) => {
      if (!dom) return;
      dom.window.dispatchEvent(new dom.window.MessageEvent("message", { data: message, source: dom.window as unknown as MessageEventSource }));
    },
    onMessage: (l) => {
      listener = l;
    },
    destroy: () => {
      listener = () => undefined;
      dom?.window.close();
      dom = null;
    },
  };
  return transport;
};

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function waitFor(condition: () => boolean, ms = 3000): Promise<void> {
  const end = Date.now() + ms;
  while (!condition()) {
    if (Date.now() > end) throw new Error("condition not met in time");
    await sleep(20);
  }
}

function run(code: string, opts: { permissions?: string[]; timeoutMs?: number } = {}) {
  const logs: string[] = [];
  const failures: Array<[string, string]> = [];
  const calls: string[] = [];
  const client = {
    call: vi.fn(async (method: string, params?: any) => {
      calls.push(method);
      if (method === "plugin.storage.get") return `stored:${params.key}`;
      return true;
    }),
  };
  const notify = vi.fn();
  const capabilities = createCapabilityHandler({
    client: client as never,
    plugin: { id: "me/p", permissions: opts.permissions ?? ["storage", "notifications"], hosts: [] },
    adapters: { ui: { notify } },
    doc: document,
  });
  const host = new SandboxHost({
    plugin: { id: "me/p", name: "P", version: "1", code },
    doc: document,
    transportFactory: jsdomTransport,
    handle: (m, p) => capabilities.handle(m, p),
    timeoutMs: opts.timeoutMs ?? 3000,
    onFailure: (kind, detail) => failures.push([kind, detail]),
    onLog: (level, text) => logs.push(`${level}:${text}`),
  });
  host.start();
  return { host, logs, failures, calls, notify, client };
}

beforeEach(() => {
  document.head.innerHTML = "";
  document.body.innerHTML = "";
});
afterEach(() => document.querySelectorAll("iframe").forEach((f) => f.remove()));

describe("sandbox iframe", () => {
  it("is created with an opaque origin and a network-less CSP", () => {
    const host = new SandboxHost({
      plugin: { id: "me/p", name: "P", version: "1", code: "1" },
      doc: document,
      transportFactory: iframeTransport,
      handle: async () => null,
      timeoutMs: 3000,
      onFailure: () => undefined,
    });
    host.start();
    const frame = document.querySelector("iframe")!;
    expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
    expect(frame.getAttribute("sandbox")).not.toContain("same-origin");
    expect(frame.srcdoc).toContain("default-src 'none'");
    expect(frame.srcdoc).not.toMatch(/connect-src|unsafe-eval/);
    host.stop();
    expect(document.querySelector("iframe")).toBeNull();
  });

  it("runs the plugin, which can use the ghostify API", async () => {
    const t = run(`
      ghostify.log.info("started");
      ghostify.storage.get("theme").then((v) => ghostify.log.info("got " + v));
      ghostify.notify("hello from plugin");
    `);
    await waitFor(() => t.logs.includes("info:got stored:theme"));
    expect(t.logs).toContain("info:started");
    expect(t.notify).toHaveBeenCalledWith("hello from plugin");
    expect(t.host.state).toBe("running");
    t.host.stop();
  });

  it("exposes only the frozen ghostify global", async () => {
    const t = run(`
      ghostify.log.info("frozen " + Object.isFrozen(ghostify) + " " + Object.isFrozen(ghostify.storage));
      try { ghostify.storage = 1; } catch (e) {}
      try { window.ghostify = null; } catch (e) {}
      ghostify.log.info("still " + typeof ghostify.storage.get);
    `);
    await waitFor(() => t.logs.some((l) => l.startsWith("info:still")));
    expect(t.logs).toContain("info:frozen true true");
    expect(t.logs).toContain("info:still function");
    t.host.stop();
  });

  it("turns a denied permission into a rejected promise, not a crash", async () => {
    const t = run(`ghostify.player.getState().catch((e) => ghostify.log.warn("denied: " + e.message));`, { permissions: ["storage"] });
    await waitFor(() => t.logs.some((l) => l.startsWith("warn:denied")));
    expect(t.logs.find((l) => l.startsWith("warn:denied"))).toContain("player.read");
    expect(t.calls).not.toContain("plugin.authorize");
    expect(t.host.state).toBe("running");
    t.host.stop();
  });

  it("an unhandled rejection is logged, not treated as a crash", async () => {
    // jsdom does not raise this event for real rejected promises, so it is dispatched by hand.
    const t = run(`window.dispatchEvent(Object.assign(new Event("unhandledrejection", { cancelable: true }), { reason: new Error("oops") }));`);
    await waitFor(() => t.logs.some((l) => l.includes("unhandled rejection")));
    expect(t.failures).toEqual([]);
    t.host.stop();
  });

  it("a thrown error while loading is a crash", async () => {
    const t = run(`throw new Error("broken plugin");`);
    await waitFor(() => t.failures.length > 0);
    expect(t.failures[0]?.[0]).toBe("crash");
    expect(t.failures[0]?.[1]).toContain("broken plugin");
    expect(t.host.state).toBe("failed");
  });

  it("delivers events and the stop signal to the plugin", async () => {
    const t = run(`
      ghostify.on("player.change", (s) => ghostify.log.info("track " + s.title));
      ghostify.on("stop", () => ghostify.log.info("stopping"));
    `);
    await waitFor(() => t.host.state === "running");
    t.host.emit("player.change", { title: "Song" });
    await waitFor(() => t.logs.includes("info:track Song"));
    t.host.stop();
  });

  it("an error inside an event handler is reported as a crash", async () => {
    const t = run(`ghostify.on("player.change", () => { throw new Error("handler broke"); });`);
    await waitFor(() => t.host.state === "running");
    t.host.emit("player.change", {});
    await waitFor(() => t.failures.length > 0);
    expect(t.failures[0]?.[1]).toContain("handler broke");
  });
});
