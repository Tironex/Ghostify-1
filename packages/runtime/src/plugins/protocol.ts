/** Messages between the host (loader) and the guest (code inside a plugin's sandbox). */

export type HostToGuest =
  | { t: "init"; id: string; name: string; version: string; code: string }
  | { t: "result"; cid: number; ok: true; value: unknown }
  | { t: "result"; cid: number; ok: false; error: string }
  | { t: "event"; name: string; payload: unknown }
  | { t: "stop" };

export type GuestToHost =
  | { t: "ready" }
  | { t: "started" }
  | { t: "hb" }
  | { t: "call"; cid: number; method: string; params: unknown }
  | { t: "error"; message: string }
  | { t: "log"; level: "debug" | "info" | "warn" | "error"; text: string };

export const MAX_MESSAGE_FIELD = 4096;

export function isGuestMessage(value: unknown): value is GuestToHost {
  if (typeof value !== "object" || value === null) return false;
  const message = value as Record<string, unknown>;
  switch (message.t) {
    case "ready":
    case "started":
    case "hb":
      return true;
    case "call":
      return Number.isSafeInteger(message.cid) && typeof message.method === "string" && message.method.length <= 64;
    case "error":
      return typeof message.message === "string";
    case "log":
      return (
        typeof message.text === "string" &&
        (message.level === "debug" || message.level === "info" || message.level === "warn" || message.level === "error")
      );
    default:
      return false;
  }
}
