import { useEffect, useMemo, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Command } from "cmdk";

import { commands, type DissectFormat } from "../lib/bindings";
import { Highlight, Palette, fuzzyMatch } from "../main_window/modals/Palette";
import styles from "./DissectAs.module.scss";

/// The value of the entry that goes back to identifying the format.
const IDENTIFY = "(identify)";

const EXACT = 1000;

/// How well `format` matches `filter`; negative when it doesn't.
function rank(format: DissectFormat, filter: string): number {
  const query = filter.toLowerCase();
  if (
    format.name === query ||
    format.extensions.some((e) => e.toLowerCase() === query)
  ) {
    return EXACT;
  }
  const title = fuzzyMatch(filter, format.title);
  const name = fuzzyMatch(filter, format.name);
  if (!title.matches && !name.matches) return -1;
  return Math.max(
    title.matches ? title.score : 0,
    name.matches ? name.score : 0,
  );
}

/// What Dissect As chooses the format of: the file, or a file within it.
export type DissectAsTarget = {
  /// Its name, whose extension suggests formats.
  name: string;
  /// The chosen format's name; null while it's identified.
  current: string | null;
};

/// Pick the format a file (or a file within it) is dissected as: those
/// listing its extension first, then every other, filtered as you type.
export function DissectAs({
  target,
  onPick,
  onClose,
  restoreFocus,
}: {
  /// Open while there is one.
  target: DissectAsTarget | null;
  onPick: (format: string | null) => void;
  onClose: () => void;
  /// Hand the focus back to the viewer once closed.
  restoreFocus: () => void;
}) {
  const open = target !== null;
  const current = target?.current ?? null;
  const [formats, setFormats] = useState<DissectFormat[] | null>(null);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState(current ?? IDENTIFY);
  const contentRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!target) return;
    setFilter("");
    setSelected(target.current ?? IDENTIFY);
    setFormats(null);
    let cancelled = false;
    commands.dissectFormats(target.name).then((list) => {
      if (!cancelled) setFormats(list);
    });
    return () => {
      cancelled = true;
    };
  }, [target]);

  const matching = useMemo(() => {
    if (!formats || !filter) return formats ?? [];
    return formats
      .map((format) => ({ format, rank: rank(format, filter) }))
      .filter((m) => m.rank >= 0)
      .sort(
        (a, b) =>
          b.rank - a.rank ||
          Number(b.format.suggested) - Number(a.format.suggested),
      )
      .map((m) => m.format);
  }, [formats, filter]);

  // The current choice may be far down the list.
  useEffect(() => {
    if (!open || !formats) return;
    contentRef.current
      ?.querySelector(`[cmdk-item][data-value="${CSS.escape(selected)}"]`)
      ?.scrollIntoView({ block: "nearest" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, formats]);

  // A filter that drops the selected entry selects the best match.
  useEffect(() => {
    if (filter && matching.length > 0) setSelected(matching[0].name);
  }, [filter, matching]);

  const item = (format: DissectFormat) => (
    <Command.Item
      key={format.name}
      value={format.name}
      onSelect={() => onPick(format.name)}
    >
      <span className={styles.check}>{format.name === current ? "✓" : ""}</span>
      <span className={styles.title}>
        <Highlight
          text={format.title}
          // An exact name or extension match isn't the title's doing.
          filter={rank(format, filter) >= EXACT ? "" : filter}
          highlightClass={styles.highlight}
        />
      </span>
      <span className={styles.meta}>
        {format.extensions.length > 0
          ? format.extensions.map((e) => `.${e}`).join(" ")
          : format.name}
      </span>
    </Command.Item>
  );

  const suggested = matching.filter((f) => f.suggested);
  return (
    <Dialog.Root open={open} onOpenChange={(o) => !o && onClose()}>
      <Dialog.Portal>
        <Dialog.Content
          ref={contentRef}
          className={styles.content}
          aria-describedby={undefined}
          onCloseAutoFocus={(e) => {
            e.preventDefault();
            restoreFocus();
          }}
        >
          <Dialog.Title className="sr-only">Dissect As</Dialog.Title>
          <Palette
            shouldFilter={false}
            label="Filter formats"
            selected={selected}
            onSelectedChange={setSelected}
          >
            <div className={styles.header}>
              <Command.Input
                value={filter}
                onValueChange={setFilter}
                placeholder={`Dissect “${target?.name ?? ""}” as… (name or extension)`}
              />
            </div>
            <Command.List label="Formats">
              {formats === null ? (
                <Command.Loading>Loading formats…</Command.Loading>
              ) : (
                <Command.Empty>No formats found</Command.Empty>
              )}
              {formats !== null && !filter && (
                <>
                  <Command.Item value={IDENTIFY} onSelect={() => onPick(null)}>
                    <span className={styles.check}>
                      {current === null ? "✓" : ""}
                    </span>
                    <span className={styles.title}>Identify automatically</span>
                  </Command.Item>
                  {suggested.length > 0 && (
                    <Command.Group heading="Matching the extension">
                      {suggested.map(item)}
                    </Command.Group>
                  )}
                  <Command.Group heading="All formats">
                    {matching.filter((f) => !f.suggested).map(item)}
                  </Command.Group>
                </>
              )}
              {filter && matching.map(item)}
            </Command.List>
          </Palette>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
