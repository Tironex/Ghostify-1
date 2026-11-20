import { useEffect, useId, useRef, useState, type ButtonHTMLAttributes, type ReactNode } from "react";
import { pickPath } from "../api/backend";

type Variant = "primary" | "secondary" | "danger";

const variants: Record<Variant, string> = {
  primary: "bg-white text-black hover:bg-zinc-200 disabled:opacity-50",
  secondary:
    "border border-zinc-300 bg-white text-zinc-800 hover:bg-zinc-100 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-100 dark:hover:bg-zinc-800",
  danger: "bg-red-600 text-white hover:bg-red-700 disabled:opacity-50",
};

export function Button({
  variant = "secondary",
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant }) {
  return (
    <button
      type="button"
      className={`inline-flex items-center justify-center rounded-md px-3 py-1.5 text-sm font-medium transition-colors disabled:cursor-not-allowed ${variants[variant]} ${className}`}
      {...props}
    />
  );
}

export function Card({ children, className = "" }: { children: ReactNode; className?: string }) {
  return (
    <section
      className={`rounded-2xl border border-zinc-200 bg-white p-4 dark:border-white/10 dark:bg-zinc-900/80 ${className}`}
    >
      {children}
    </section>
  );
}

const tones = {
  neutral: "bg-zinc-100 text-zinc-700 dark:bg-zinc-800 dark:text-zinc-300",
  good: "bg-zinc-200 text-zinc-900 dark:bg-white/12 dark:text-white",
  warn: "bg-amber-100 text-amber-900 dark:bg-amber-950 dark:text-amber-300",
  bad: "bg-red-100 text-red-800 dark:bg-red-950 dark:text-red-300",
  accent: "bg-zinc-200 text-zinc-900 dark:bg-white dark:text-black",
};

export function Badge({ tone = "neutral", children }: { tone?: keyof typeof tones; children: ReactNode }) {
  return <span className={`inline-block rounded-full px-2 py-0.5 text-xs font-medium ${tones[tone]}`}>{children}</span>;
}

export function PageHeader({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <header className="mb-5 flex flex-wrap items-center justify-between gap-3">
      <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
      <div className="flex items-center gap-2">{children}</div>
    </header>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return (
    <p className="rounded-xl border border-dashed border-zinc-300 p-8 text-center text-sm text-zinc-500 dark:border-zinc-700">
      {children}
    </p>
  );
}

export function ErrorNote({ error, retry }: { error: string; retry?: () => void }) {
  return (
    <div role="alert" className="rounded-xl border border-red-300 bg-red-50 p-4 text-sm text-red-900 dark:border-red-900 dark:bg-red-950 dark:text-red-200">
      <p>{error}</p>
      {retry && (
        <Button className="mt-2" onClick={retry}>
          Try again
        </Button>
      )}
    </div>
  );
}

export function Loading() {
  return (
    <p role="status" className="text-sm text-zinc-500">
      Loading…
    </p>
  );
}

export function Dialog({
  title,
  children,
  onClose,
  actions,
}: {
  title: string;
  children: ReactNode;
  onClose: () => void;
  actions: ReactNode;
}) {
  const titleId = useId();
  const ref = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    // Focus the dialog once when it opens, never again while the user types.
    ref.current?.focus();
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && close.current();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/50 p-4">
      <div
        ref={ref}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        className="w-full max-w-md rounded-xl border border-zinc-200 bg-white p-5 shadow-xl dark:border-zinc-700 dark:bg-zinc-900"
      >
        <h2 id={titleId} className="mb-3 text-lg font-semibold">
          {title}
        </h2>
        <div className="mb-5 space-y-3 text-sm">{children}</div>
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>Cancel</Button>
          {actions}
        </div>
      </div>
    </div>
  );
}

export function PathField({
  label,
  value,
  onChange,
  directory,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  directory?: boolean;
  placeholder?: string;
}) {
  const id = useId();
  return (
    <div>
      <label htmlFor={id} className="mb-1 block text-sm font-medium">
        {label}
      </label>
      <div className="flex gap-2">
        <input
          id={id}
          value={value}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          className="min-w-0 flex-1 rounded-md border border-zinc-300 bg-white px-2 py-1.5 text-sm dark:border-zinc-700 dark:bg-zinc-950"
        />
        <Button
          onClick={async () => {
            const picked = await pickPath({ directory });
            if (picked) onChange(picked);
          }}
        >
          Browse…
        </Button>
      </div>
    </div>
  );
}

export function Switch({ checked, onChange, label }: { checked: boolean; onChange: (value: boolean) => void; label: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
      className={`relative h-6 w-11 shrink-0 rounded-full transition-colors ${checked ? "bg-accent" : "bg-zinc-300 dark:bg-zinc-700"}`}
    >
      <span className={`absolute top-0.5 left-0.5 h-5 w-5 rounded-full bg-white transition-transform ${checked ? "translate-x-5 bg-black" : ""}`} />
    </button>
  );
}

/** Runs an async action, tracks the busy state and reports failures through `onError`. */
export function useAction(onError: (error: unknown) => void) {
  const [busy, setBusy] = useState(false);
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } catch (error) {
      onError(error);
    } finally {
      setBusy(false);
    }
  };
  return { busy, run };
}

export const permissionLabels: Record<string, string> = {
  "player.read": "See what is playing",
  "player.control": "Control playback",
  "playlist.read": "Read your playlists",
  "playlist.write": "Change your playlists",
  storage: "Store its own settings",
  network: "Connect to the internet (its listed hosts)",
  ui: "Change the Spotify interface",
  notifications: "Show notifications",
  clipboard: "Use the clipboard",
};
