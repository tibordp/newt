import { useEffect, useRef } from "react";
import styles from "./ConnectionLog.module.scss";

export default function ConnectionLog({ log }: { log: string[] }) {
  const ref = useRef<HTMLPreElement>(null);

  useEffect(() => {
    if (ref.current) {
      ref.current.scrollTop = ref.current.scrollHeight;
    }
  }, [log.length]);

  return (
    <pre className={styles.log} ref={ref}>
      {log.join("\n")}
    </pre>
  );
}
