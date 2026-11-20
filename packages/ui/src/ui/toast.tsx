import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";
import { errorText } from "../api/backend";

type Kind = "info" | "error";
interface Toast {
  id: number;
  kind: Kind;
  text: string;
}

interface ToastApi {
  info: (text: string) => void;
  error: (error: unknown) => void;
}

const Context = createContext<ToastApi | null>(null);
let next = 1;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const push = useCallback((kind: Kind, text: string) => {
    const id = next++;
    setToasts((list) => [...list.slice(-3), { id, kind, text }]);
    window.setTimeout(() => setToasts((list) => list.filter((t) => t.id !== id)), kind === "error" ? 9000 : 4500);
  }, []);
  const api = useMemo<ToastApi>(
    () => ({ info: (text) => push("info", text), error: (e) => push("error", errorText(e)) }),
    [push],
  );
  return (
    <Context.Provider value={api}>
      {children}
      <div className="fixed right-4 bottom-4 z-50 flex w-80 flex-col gap-2" role="region" aria-label="Notifications">
        {toasts.map((t) => (
          <div
            key={t.id}
            role={t.kind === "error" ? "alert" : "status"}
            className={
              "rounded-lg border px-3 py-2 text-sm shadow-lg " +
              (t.kind === "error"
                ? "border-red-300 bg-red-50 text-red-900 dark:border-red-900 dark:bg-red-950 dark:text-red-200"
                : "border-zinc-200 bg-white text-zinc-800 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-100")
            }
          >
            {t.text}
          </div>
        ))}
      </div>
    </Context.Provider>
  );
}

export function useToast(): ToastApi {
  const api = useContext(Context);
  if (!api) throw new Error("useToast needs a ToastProvider");
  return api;
}
