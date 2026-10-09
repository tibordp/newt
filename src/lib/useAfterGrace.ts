import { useEffect, useState } from "react";

/// How long work may run before it shows as in progress: shorter work
/// would only flash an indicator on and off.
export const BUSY_GRACE_MS = 200;

/// `active`, once it has stayed true for `BUSY_GRACE_MS`; false as soon as
/// it isn't.
export function useAfterGrace(active: boolean): boolean {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    if (!active) {
      setShown(false);
      return;
    }
    const timeout = setTimeout(() => setShown(true), BUSY_GRACE_MS);
    return () => clearTimeout(timeout);
  }, [active]);
  return active && shown;
}
