import { render } from "@testing-library/react";
import type { ReactElement } from "react";
import { BackendError, setTransport } from "./api/backend";
import { ToastProvider } from "./ui/toast";

export type Handler = (params: any) => unknown;

export interface Recorder {
  calls: { method: string; params: any }[];
  of: (method: string) => any[];
}

/** Installs a transport that answers from `handlers`; unknown methods fail loudly. */
export function backend(handlers: Record<string, Handler>): Recorder {
  const calls: Recorder["calls"] = [];
  setTransport(async (method, params) => {
    calls.push({ method, params });
    const handler = handlers[method];
    if (!handler) throw new BackendError("not_found", `no handler for ${method}`);
    return handler(params);
  });
  return { calls, of: (method) => calls.filter((c) => c.method === method).map((c) => c.params) };
}

export function fail(code: string, message: string): never {
  throw new BackendError(code, message);
}

export function renderWithToasts(ui: ReactElement) {
  return render(<ToastProvider>{ui}</ToastProvider>);
}
