// The only place that knows how to reach the Rust side.

export class BackendError extends Error {
  constructor(
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "BackendError";
  }
}

export type Transport = (method: string, params: unknown) => Promise<unknown>;

interface TauriGlobal {
  core?: { invoke?: (command: string, args: Record<string, unknown>) => Promise<unknown> };
  dialog?: { open?: (options: Record<string, unknown>) => Promise<string | string[] | null> };
}

declare global {
  interface Window {
    __TAURI__?: TauriGlobal;
  }
}

let override: Transport | null = null;

/** Replaces the transport (tests); `null` restores the default. */
export function setTransport(transport: Transport | null): void {
  override = transport;
}

function defaultTransport(): Transport | null {
  const invoke = window.__TAURI__?.core?.invoke;
  return invoke ? (method, params) => invoke("ghostify_call", { method, params }) : null;
}

function normalise(error: unknown): BackendError {
  if (error instanceof BackendError) return error;
  if (typeof error === "object" && error !== null) {
    const { code, message } = error as { code?: unknown; message?: unknown };
    if (typeof code === "string" && typeof message === "string") return new BackendError(code, message);
  }
  return new BackendError("internal", typeof error === "string" ? error : "Unexpected error");
}

export async function call<T>(method: string, params: unknown = null): Promise<T> {
  const transport = override ?? defaultTransport();
  if (!transport) {
    throw new BackendError("unavailable", "Ghostify has to run inside the desktop app.");
  }
  try {
    return (await transport(method, params)) as T;
  } catch (error) {
    throw normalise(error);
  }
}

/** Shows or hides the mini player window (desktop shell only). */
export async function toggleMiniPlayer(): Promise<void> {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) throw new BackendError("unavailable", "The mini player needs the desktop app.");
  try {
    await invoke("toggle_mini_player", {});
  } catch (error) {
    throw normalise(error);
  }
}

/** Opens the system file dialog. Returns `null` if cancelled or unavailable. */
export async function pickPath(options: { directory?: boolean } = {}): Promise<string | null> {
  const open = window.__TAURI__?.dialog?.open;
  if (!open) return null;
  const picked = await open({ directory: options.directory ?? false, multiple: false });
  return typeof picked === "string" ? picked : null;
}

export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
