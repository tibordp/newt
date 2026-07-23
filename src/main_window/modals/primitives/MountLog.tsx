import { useEffect, useRef } from "react";
import styles from "./MountLog.module.scss";

/// Streaming connect/bootstrap log, shown while a mount is in flight.
/// Failures don't need it live — the error message carries the transcript.
export function MountLogView({
  lines,
  visible,
}: {
  lines?: string[];
  visible: boolean;
}) {
  const boxRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = boxRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);
  if (!visible || !lines || lines.length === 0) return null;
  return (
    <div ref={boxRef} className={styles.mountLog}>
      {lines.map((l, i) => (
        <div key={i}>{l}</div>
      ))}
    </div>
  );
}
