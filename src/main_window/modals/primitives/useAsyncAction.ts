import { useCallback, useRef, useState } from "react";

export type AsyncActionState = {
  pending: boolean;
  error: string | null;
};

export type AsyncAction<Args extends unknown[]> = AsyncActionState & {
  // Runs the action. Resolves to true on success, false on failure (error
  // captured in state) or if a previous run is still in flight.
  run: (...args: Args) => Promise<boolean>;
  clearError: () => void;
};

// Wraps an async function with pending/error state tracking suitable for
// driving a dialog submit button. Errors thrown by `fn` (or returned as a
// string from a tryCommand-style call) are captured into `error` and the
// promise resolves to `false` instead of throwing — so callers can do
// `if (await run()) closeModal()` without try/catch.
//
// The action is single-flight: while one run is in progress, additional
// calls are ignored. `error` is cleared at the start of each run.
export function useAsyncAction<Args extends unknown[]>(
  fn: (...args: Args) => Promise<string | null | void>,
): AsyncAction<Args> {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);

  const run = useCallback(
    async (...args: Args): Promise<boolean> => {
      if (inFlight.current) return false;
      inFlight.current = true;
      setPending(true);
      setError(null);
      try {
        const result = await fn(...args);
        if (typeof result === "string") {
          setError(result);
          return false;
        }
        return true;
      } catch (e) {
        setError(String(e));
        return false;
      } finally {
        inFlight.current = false;
        setPending(false);
      }
    },
    [fn],
  );

  const clearError = useCallback(() => setError(null), []);

  return { pending, error, run, clearError };
}
