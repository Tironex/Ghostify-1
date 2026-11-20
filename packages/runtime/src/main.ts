import { readConfig } from "./loader";
import { startLoader } from "./loader";

declare global {
  interface Window {
    __GHOSTIFY__?: unknown;
    Ghostify?: { version: string; stop(): void };
  }
}

function boot(): void {
  const config = readConfig(window.__GHOSTIFY__);
  if (!config) {
    console.error("[ghostify] missing or invalid configuration; is ghostify/config.js loaded?");
    return;
  }
  if (window.Ghostify) return; // already running (hot reload of the bundle)
  const loader = startLoader(config, document);
  window.Ghostify = { version: config.version, stop: () => loader.stop() };
}

boot();
