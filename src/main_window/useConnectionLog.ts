import { useEffect, useState } from "react";
import { commands, type LogCursor } from "../lib/bindings";

type Fetched = { generation: number; end: number; lines: string[] };

/// The connection log's lines. Published state carries only `cursor`; the
/// lines it has grown by are fetched from Rust as it moves.
export function useConnectionLog(cursor: LogCursor): string[] {
  const [log, setLog] = useState<Fetched>(() => ({
    generation: cursor.generation,
    end: 0,
    lines: [],
  }));

  useEffect(() => {
    if (cursor.generation === log.generation && cursor.end === log.end) {
      return;
    }
    let current = true;
    void commands.connectionLog(log.generation, log.end).then((chunk) => {
      if (!current) return;
      const end = chunk.start + chunk.lines.length;
      // A new log, or lines dropped past the cap: start over from what came.
      const continues =
        chunk.generation === log.generation && chunk.start === log.end;
      setLog({
        generation: chunk.generation,
        end,
        lines: continues ? [...log.lines, ...chunk.lines] : chunk.lines,
      });
    });
    return () => {
      current = false;
    };
  }, [cursor.generation, cursor.end, log]);

  return log.lines;
}
