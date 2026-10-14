import { useEffect, useRef, useState } from "react";
import { commands, type ConnectionStatusWire } from "../lib/bindings";
import { safe } from "../lib/ipc";
import { useAfterGrace } from "../lib/useAfterGrace";
import styles from "./ConnectionScreen.module.scss";
import Spinner from "./Spinner";
import { useConnectionLog } from "./useConnectionLog";

/** What the main window shows while its session isn't connected: the
 * connection's transcript, and a status bar saying where it stands. */
export default function ConnectionScreen({
  status,
}: {
  status: Exclude<ConnectionStatusWire, { status: "connected" }>;
}) {
  const logRef = useRef<HTMLPreElement>(null);
  const log = useConnectionLog(status.log);

  useEffect(() => {
    if (logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [log.length]);

  const connecting = useAfterGrace(status.status === "connecting");
  const elapsed = useElapsed(status.status === "connecting");

  return (
    <div className={styles.screen}>
      <pre className={styles.log} ref={logRef}>
        {log.join("\n")}
        {status.status === "connecting" && (
          <>
            {log.length > 0 && "\n"}
            <span className={styles.cursor} aria-hidden />
          </>
        )}
      </pre>
      {status.status === "connecting" ? (
        <div className={styles.statusbar} role="status">
          {connecting && (
            <>
              <Spinner />
              <span className={styles.message}>{status.message}</span>
              <span className={styles.elapsed}>{formatElapsed(elapsed)}</span>
            </>
          )}
        </div>
      ) : (
        <div className={`${styles.statusbar} ${styles.error}`} role="alert">
          <span className={styles.message}>{status.error}</span>
          {/* The session is gone: reconnecting is the one thing left to
              do, so Enter does it. */}
          <button autoFocus onClick={() => safe(commands.reconnect())}>
            Reconnect
          </button>
        </div>
      )}
    </div>
  );
}

/** Whole seconds `running` has been true, counting while it is. */
function useElapsed(running: boolean): number {
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    setSeconds(0);
    if (!running) return;
    const start = performance.now();
    const tick = setInterval(
      () => setSeconds(Math.floor((performance.now() - start) / 1000)),
      1000,
    );
    return () => clearInterval(tick);
  }, [running]);
  return seconds;
}

function formatElapsed(seconds: number): string {
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}
