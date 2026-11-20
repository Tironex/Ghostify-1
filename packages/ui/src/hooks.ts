import { useCallback, useEffect, useRef, useState } from "react";
import { errorText } from "./api/backend";

export interface Loaded<T> {
  data: T | null;
  error: string | null;
  loading: boolean;
  reload: () => void;
}

/** Runs `load` on mount and whenever `reload` is called. Stale answers are dropped. */
export function useLoad<T>(load: () => Promise<T>, deps: readonly unknown[] = []): Loaded<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  const latest = useRef(load);
  latest.current = load;

  useEffect(() => {
    let current = true;
    setLoading(true);
    latest
      .current()
      .then((value) => {
        if (!current) return;
        setData(value);
        setError(null);
      })
      .catch((e: unknown) => {
        if (current) setError(errorText(e));
      })
      .finally(() => {
        if (current) setLoading(false);
      });
    return () => {
      current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tick, ...deps]);

  const reload = useCallback(() => setTick((n) => n + 1), []);
  return { data, error, loading, reload };
}
