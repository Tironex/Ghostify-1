import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { bridgeUrls, readConfig } from "../src/config";
import { startLoader } from "../src/loader";
import { SCHEME_ATTRIBUTE, STYLE_ELEMENT_ID, ThemeApplier } from "../src/theme";
import { FakeSocket, flush } from "./fake";

const SECRET = "k".repeat(64);
const config = { ports: [49421, 49422], secret: SECRET, version: "0.1.0" };

beforeEach(() => {
  vi.useFakeTimers();
  FakeSocket.reset();
  document.head.innerHTML = "";
  document.documentElement.removeAttribute(SCHEME_ATTRIBUTE);
});
afterEach(() => vi.useRealTimers());

describe("readConfig", () => {
  it("accepts a valid configuration", () => {
    expect(readConfig(config)).toEqual(config);
    expect(readConfig({ ports: [1], secret: SECRET })?.version).toBe("unknown");
  });

  it("rejects invalid ones", () => {
    for (const bad of [
      null,
      "x",
      {},
      { ports: [], secret: SECRET },
      { ports: [0], secret: SECRET },
      { ports: [70000], secret: SECRET },
      { ports: [1.5], secret: SECRET },
      { ports: [1], secret: "short" },
      { ports: [1], secret: 5 },
    ]) {
      expect(readConfig(bad)).toBeNull();
    }
  });

  it("builds loopback urls", () => {
    expect(bridgeUrls(config)).toEqual(["ws://127.0.0.1:49421/", "ws://127.0.0.1:49422/"]);
  });
});

describe("ThemeApplier", () => {
  it("creates, updates and removes the style element", () => {
    const theme = new ThemeApplier(document);
    theme.applyCss("a{color:red}");
    expect(document.querySelectorAll(`#${STYLE_ELEMENT_ID}`)).toHaveLength(1);
    expect(theme.currentCss()).toBe("a{color:red}");
    theme.applyCss("a{color:blue}");
    expect(document.querySelectorAll(`#${STYLE_ELEMENT_ID}`)).toHaveLength(1);
    expect(theme.currentCss()).toBe("a{color:blue}");
    theme.applyCss("");
    expect(document.getElementById(STYLE_ELEMENT_ID)).toBeNull();
    expect(theme.currentCss()).toBe("");
    theme.applyCss("");
  });

  it("sets the scheme attribute", () => {
    new ThemeApplier(document).applyScheme("light");
    expect(document.documentElement.getAttribute(SCHEME_ATTRIBUTE)).toBe("light");
  });
});

describe("startLoader", () => {
  function boot() {
    const loader = startLoader(config, document, {
      socketFactory: (url) => new FakeSocket(url),
      random: () => 1,
    });
    FakeSocket.last.authenticate();
    return loader;
  }

  it("says hello and syncs the current theme after connecting", async () => {
    const loader = boot();
    await flush();
    const socket = FakeSocket.last;
    const hello = socket.sent.find((m) => m.method === "session.hello")!;
    expect(hello.params).toEqual({ client: "ghostify-loader", version: "0.1.0" });
    socket.reply(hello.id, {});
    await flush();
    await flush();
    const current = socket.sent.find((m) => m.method === "theme.current")!;
    socket.reply(current.id, { css: "body{background:#000}", scheme: "dark" });
    await flush();
    await flush();
    expect(loader.theme.currentCss()).toBe("body{background:#000}");
    expect(document.documentElement.getAttribute(SCHEME_ATTRIBUTE)).toBe("dark");
    loader.stop();
  });

  it("tolerates a server without theme.current", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const loader = boot();
    await flush();
    const socket = FakeSocket.last;
    socket.reply(socket.sent.find((m) => m.method === "session.hello")!.id, {});
    await flush();
    await flush();
    socket.fail(socket.sent.find((m) => m.method === "theme.current")!.id, -32601, "unknown method");
    await flush();
    await flush();
    expect(warn).not.toHaveBeenCalled();
    expect(loader.theme.currentCss()).toBe("");
    warn.mockRestore();
    loader.stop();
  });

  it("live reloads on theme.css and theme.scheme notifications", async () => {
    const loader = boot();
    await flush();
    const socket = FakeSocket.last;
    socket.push("theme.css", { css: "a{}" });
    expect(loader.theme.currentCss()).toBe("a{}");
    socket.push("theme.css", { css: "b{}" });
    expect(loader.theme.currentCss()).toBe("b{}");
    socket.push("theme.scheme", { scheme: "light" });
    expect(document.documentElement.getAttribute(SCHEME_ATTRIBUTE)).toBe("light");
    // invalid payloads are ignored
    socket.push("theme.css", { css: 5 });
    socket.push("theme.css", null);
    socket.push("theme.scheme", { scheme: "purple" });
    expect(loader.theme.currentCss()).toBe("b{}");
    expect(document.documentElement.getAttribute(SCHEME_ATTRIBUTE)).toBe("light");
    loader.stop();
  });

  it("keeps the last theme while the bridge is down", async () => {
    const loader = boot();
    await flush();
    FakeSocket.last.push("theme.css", { css: "keep{}" });
    FakeSocket.last.close();
    expect(loader.client.connectionState).toBe("closed");
    expect(loader.theme.currentCss()).toBe("keep{}");
    loader.stop();
  });
});
