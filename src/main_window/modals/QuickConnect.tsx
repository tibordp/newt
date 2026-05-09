import { useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Command } from "cmdk";

import { commands, type ConnectionProfile } from "../../lib/bindings";
import { safe, safeSilent } from "../../lib/ipc";
import { MainWindowState } from "../types";
import { Palette, Highlight, fuzzyMatch } from "./Palette";
import styles from "./HotPaths.module.scss";

type QuickConnectProps = {
  connections: ConnectionProfile[];
  state: MainWindowState | null;
};

const TYPE_LABELS: Record<ConnectionProfile["type"], string> = {
  s3: "S3",
  sftp: "SFTP",
  remote: "Remote",
};

const preventAutoFocus = (e: Event) => e.preventDefault();

function subtitle(c: ConnectionProfile): string {
  const parts = [TYPE_LABELS[c.type] || c.type];
  if (c.type === "s3") {
    if (c.bucket) parts.push(c.bucket);
    if (c.region) parts.push(c.region);
    if (c.endpoint_url) parts.push(c.endpoint_url);
  } else {
    parts.push(c.host);
  }
  return parts.join(" \u2014 ");
}

function searchableText(c: ConnectionProfile): string {
  const fields: (string | null | undefined)[] = [c.name, c.id];
  if (c.type === "s3") {
    fields.push(c.bucket, c.region, c.endpoint_url);
  } else {
    fields.push(c.host);
  }
  return fields.filter(Boolean).join(" ");
}

export default function QuickConnect({
  connections,
  state,
}: QuickConnectProps) {
  const [filter, setFilter] = useState("");
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  const paneHandle =
    state?.display_options.panes_focused && state?.display_options.active_pane;

  const filtered = useMemo(() => {
    return connections
      .map((c) => {
        const text = searchableText(c);
        const result = fuzzyMatch(filter, text);
        return { connection: c, ...result };
      })
      .filter(({ matches }) => matches)
      .sort((a, b) => b.score - a.score);
  }, [connections, filter]);

  const onSelect = (value: string) => {
    if (pendingDelete !== null) return;
    safe(
      commands.connectProfile(
        typeof paneHandle === "number" ? paneHandle : 0,
        value,
      ),
    );
  };

  const requestDelete = (id: string, e?: React.MouseEvent) => {
    e?.stopPropagation();
    e?.preventDefault();
    setPendingDelete(id);
  };

  const confirmDelete = (id: string) => {
    safeSilent(commands.cmdDeleteConnection(id)).then(() => {
      // Re-open to refresh the list
      commands.dialog(
        "quick_connect",
        typeof paneHandle === "number" ? paneHandle : null,
      );
    });
    setPendingDelete(null);
  };

  const cancelDelete = () => setPendingDelete(null);

  const getSelectedId = (): string | null => {
    const el = document.querySelector('[cmdk-item][data-selected="true"]');
    return el?.getAttribute("data-value") ?? null;
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (pendingDelete !== null) {
      if (e.key === "Enter" || e.key === "y" || e.key === "Y") {
        e.preventDefault();
        e.stopPropagation();
        confirmDelete(pendingDelete);
      } else if (e.key === "n" || e.key === "N" || e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        cancelDelete();
      } else {
        e.preventDefault();
        e.stopPropagation();
      }
      return;
    }

    if (e.key === "Delete") {
      const id = getSelectedId();
      if (id) {
        e.preventDefault();
        requestDelete(id);
      }
    }
  };

  return (
    <Dialog.Content
      className={styles.content}
      onCloseAutoFocus={preventAutoFocus}
      onEscapeKeyDown={(e) => {
        if (pendingDelete !== null) {
          e.preventDefault();
          cancelDelete();
        }
      }}
    >
      <Dialog.Title className="sr-only">Quick Connect</Dialog.Title>
      <Palette shouldFilter={false} onKeyDown={onKeyDown}>
        <div className={styles.header}>
          <Command.Input
            value={filter}
            onValueChange={setFilter}
            placeholder="Search connections..."
          />
        </div>
        <Command.List>
          <Command.Empty>
            {connections.length === 0
              ? "No saved connections. Use the connect or mount dialogs to save one."
              : "No matching connections."}
          </Command.Empty>
          {filtered.map(({ connection: c }) => {
            const isConfirming = pendingDelete === c.id;
            return (
              <Command.Item key={c.id} value={c.id} onSelect={onSelect}>
                {isConfirming ? (
                  <div className={styles.confirmRow}>
                    <span>Remove connection?</span>
                    <span className={styles.confirmActions}>
                      <button
                        className={styles.confirmYes}
                        onClick={(e) => {
                          e.stopPropagation();
                          confirmDelete(c.id);
                        }}
                        tabIndex={-1}
                      >
                        Yes
                      </button>
                      <button
                        className={styles.confirmNo}
                        onClick={(e) => {
                          e.stopPropagation();
                          cancelDelete();
                        }}
                        tabIndex={-1}
                      >
                        No
                      </button>
                    </span>
                  </div>
                ) : (
                  <>
                    <div className={styles.itemContent}>
                      <span className={styles.name}>
                        <Highlight
                          text={c.name}
                          filter={filter}
                          highlightClass={styles.highlight}
                        />
                      </span>
                      <span className={styles.path}>
                        <Highlight
                          text={subtitle(c)}
                          filter={filter}
                          highlightClass={styles.highlight}
                        />
                      </span>
                    </div>
                    <button
                      className={styles.deleteBtn}
                      onClick={(e) => requestDelete(c.id, e)}
                      title="Remove connection"
                      tabIndex={-1}
                    >
                      &times;
                    </button>
                  </>
                )}
              </Command.Item>
            );
          })}
        </Command.List>
      </Palette>
    </Dialog.Content>
  );
}
