import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "./api";

export interface Resource<T> {
  data: T | undefined;
  error: string | undefined;
  loading: boolean;
  /** Re-run the fetch, keeping the previous data visible while it runs. */
  reload(): void;
}

/**
 * Fetch once per key change, with the in-flight request cancelled if the key
 * changes again first — otherwise a slow earlier response can land after a
 * faster later one and overwrite it.
 */
export function useResource<T>(
  fetcher: (signal: AbortSignal) => Promise<T>,
  deps: unknown[]
): Resource<T> {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState(true);
  const [nonce, setNonce] = useState(0);

  // Keep the latest fetcher without making it a dependency: it is usually an
  // inline closure, which would re-run the effect on every render.
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;

  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    setLoading(true);
    fetcherRef
      .current(controller.signal)
      .then((result) => {
        if (cancelled) return;
        setData(result);
        setError(undefined);
      })
      .catch((err: unknown) => {
        if (cancelled || controller.signal.aborted) return;
        setError(describeError(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
      controller.abort();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce]);

  const reload = useCallback(() => setNonce((n) => n + 1), []);

  return { data, error, loading, reload };
}

export function describeError(err: unknown): string {
  if (err instanceof ApiError) return err.message;
  if (err instanceof Error) return err.message;
  return String(err);
}

/** Debounce a rapidly-changing value, e.g. a search box driving a request. */
export function useDebounced<T>(value: T, ms = 250): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return debounced;
}

/** Poll a resource on an interval, pausing when the tab is hidden. */
export function usePoll(reload: () => void, intervalMs: number, active = true) {
  useEffect(() => {
    if (!active) return;
    const tick = () => {
      if (document.visibilityState === "visible") reload();
    };
    const id = setInterval(tick, intervalMs);
    return () => clearInterval(id);
  }, [reload, intervalMs, active]);
}
