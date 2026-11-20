/** Shape of `window.__GHOSTIFY__`, written by `ghostify/config.js` at patch time. */
export interface GhostifyConfig {
  ports: number[];
  secret: string;
  version: string;
}

export function readConfig(source: unknown): GhostifyConfig | null {
  if (typeof source !== "object" || source === null) return null;
  const { ports, secret, version } = source as Record<string, unknown>;
  if (!Array.isArray(ports) || ports.length === 0) return null;
  if (!ports.every((p) => Number.isInteger(p) && (p as number) > 0 && (p as number) < 65536)) return null;
  if (typeof secret !== "string" || secret.length < 32) return null;
  return { ports: ports as number[], secret, version: typeof version === "string" ? version : "unknown" };
}

export function bridgeUrls(config: GhostifyConfig): string[] {
  return config.ports.map((port) => `ws://127.0.0.1:${port}/`);
}
