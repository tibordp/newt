import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Command } from "cmdk";
import { safe } from "../../lib/ipc";
import {
  commands,
  type QuickOpenHit,
  type QuickOpenUpdate,
} from "../../lib/bindings";
import { fileIconGlyph } from "../../lib/fileIcons";
import { Palette } from "./Palette";
import styles from "./QuickOpen.module.scss";

const preventAutoFocus = (e: Event) => e.preventDefault();

const count = (n: number) => n.toLocaleString();

/// `chars` with the characters at `highlights` (indices into the whole
/// path, from `offset`) in bold.
function Highlighted({
  chars,
  offset,
  highlights,
}: {
  chars: string[];
  offset: number;
  highlights: Set<number>;
}) {
  const runs: { text: string; bold: boolean }[] = [];
  chars.forEach((ch, i) => {
    const bold = highlights.has(offset + i);
    const last = runs[runs.length - 1];
    if (last && last.bold === bold) last.text += ch;
    else runs.push({ text: ch, bold });
  });
  return (
    <>
      {runs.map((run, i) =>
        run.bold ? (
          <b key={i} className={styles.highlight}>
            {run.text}
          </b>
        ) : (
          run.text
        ),
      )}
    </>
  );
}

function Hit({ hit }: { hit: QuickOpenHit }) {
  const chars = Array.from(hit.rel);
  const slash = chars.lastIndexOf("/");
  const name = chars.slice(slash + 1);
  const dir = chars.slice(0, Math.max(slash, 0));
  const highlights = new Set(hit.highlights);
  const { ch, color } = fileIconGlyph(name.join(""));
  return (
    <>
      {hit.is_dir ? (
        <div className="file-icon folder" aria-hidden />
      ) : (
        <div
          className="file-icon"
          style={{ "--icon-color": color } as React.CSSProperties}
          aria-hidden
        >
          {ch}
        </div>
      )}
      <span className={styles.name}>
        <Highlighted chars={name} offset={slash + 1} highlights={highlights} />
      </span>
      {dir.length > 0 && (
        <span className={styles.dir}>
          <Highlighted chars={dir} offset={0} highlights={highlights} />
        </span>
      )}
    </>
  );
}

function status(update: QuickOpenUpdate | null): string {
  if (!update) return "Searching…";
  if (update.error) return update.error;
  const parts = [
    update.walking
      ? `Searching… ${count(update.walked)} items`
      : update.truncated
        ? `Stopped at ${count(update.walked)} items`
        : `${count(update.walked)} items`,
  ];
  if (update.unreadable > 0) {
    parts.push(
      update.unreadable === 1
        ? "1 folder could not be read"
        : `${count(update.unreadable)} folders could not be read`,
    );
  }
  return parts.join(" · ");
}

export default function QuickOpen({
  rootDisplay,
  update,
}: {
  rootDisplay: string;
  update: QuickOpenUpdate | null;
}) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState("");
  const results = update?.results ?? [];

  // The selection follows its path while the walk reorders the results;
  // a new query's results start from the top.
  const shownQuery = useRef<string | null>(null);
  useEffect(() => {
    if (!update) return;
    const newQuery = shownQuery.current !== update.query;
    shownQuery.current = update.query;
    if (newQuery || !update.results.some((h) => h.rel === selected)) {
      setSelected(update.results[0]?.rel ?? "");
    }
    // `selected` is read, not followed: only new results move it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [update]);

  const onQueryChange = (q: string) => {
    setQuery(q);
    void commands.quickOpenQuery(q);
  };

  return (
    <Dialog.Content
      className={styles.content}
      onCloseAutoFocus={preventAutoFocus}
    >
      <Dialog.Title className="sr-only">Go to File</Dialog.Title>
      <Palette
        shouldFilter={false}
        label="Go to file"
        selected={selected}
        onSelectedChange={setSelected}
      >
        <div className={styles.header}>
          <Command.Input
            value={query}
            onValueChange={onQueryChange}
            placeholder={`Go to file in ${rootDisplay}`}
          />
        </div>
        <div className={styles.status}>{status(update)}</div>
        <Command.List label="Files">
          {update && !update.walking && (
            <Command.Empty>No matching files</Command.Empty>
          )}
          {results.map((hit) => (
            <Command.Item
              key={hit.rel}
              value={hit.rel}
              onSelect={() => safe(commands.quickOpenAccept(hit.rel))}
            >
              <Hit hit={hit} />
            </Command.Item>
          ))}
        </Command.List>
      </Palette>
    </Dialog.Content>
  );
}
