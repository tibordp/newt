import { useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Command } from "cmdk";

import {
  commands,
  type ConnectionKind,
  type ConnectionProfile,
  type RecentConnection,
} from "../../lib/bindings";
import { safe, safeSilent } from "../../lib/ipc";
import { MainWindowState } from "../types";
import { Palette, Highlight, fuzzyMatch } from "./Palette";
import styles from "./HotPaths.module.scss";

type QuickConnectProps = {
  connections: ConnectionProfile[];
  recentConnections: RecentConnection[];
  state: MainWindowState | null;
};

const TYPE_LABELS: Record<ConnectionKind["type"], string> = {
  s3: "S3",
  sftp: "SFTP",
  ssh: "SSH",
  docker: "Docker",
  podman: "Podman",
  kube: "Kubernetes",
  custom: "Custom",
};

const preventAutoFocus = (e: Event) => e.preventDefault();

// Reads only the (flattened) kind fields, so it works for both a saved
// profile and a recent connection.
function connectionDetail(c: ConnectionKind): string {
  switch (c.type) {
    case "s3": {
      const parts: string[] = [];
      if (c.bucket) parts.push(c.bucket);
      if (c.region) parts.push(c.region);
      if (c.endpoint_url) parts.push(c.endpoint_url);
      return parts.join(" / ");
    }
    case "sftp":
    case "ssh":
      return c.host;
    case "docker":
    case "podman":
      return c.user ? `${c.user}@${c.container}` : c.container;
    case "kube": {
      const ns = c.namespace ? `${c.namespace}/` : "";
      return c.container ? `${ns}${c.pod}:${c.container}` : `${ns}${c.pod}`;
    }
    case "custom":
      return c.command;
  }
}

function subtitle(c: ConnectionProfile): string {
  const parts = [TYPE_LABELS[c.type] || c.type];
  const detail = connectionDetail(c);
  if (detail) parts.push(detail);
  // Pane-scoped spawn profiles mount into the active pane instead of
  // opening a session window; flag them so selection isn't a surprise.
  if (c.open_in === "pane" && c.type !== "s3" && c.type !== "sftp") {
    parts.push("pane mount");
  }
  return parts.join(" — ");
}

function recentLabel(rc: RecentConnection): string {
  return connectionDetail(rc) || TYPE_LABELS[rc.type];
}

function recentSubtitle(rc: RecentConnection): string {
  const parts = [TYPE_LABELS[rc.type] || rc.type];
  if (rc.open_in === "pane" && rc.type !== "sftp") parts.push("pane mount");
  return parts.join(" — ");
}

export default function QuickConnect({
  connections,
  recentConnections,
  state,
}: QuickConnectProps) {
  const [filter, setFilter] = useState("");
  // Holds the cmdk value of the row pending delete-confirmation, or null.
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  const paneHandle =
    state?.display_options.panes_focused && state?.display_options.active_pane;
  const ph = typeof paneHandle === "number" ? paneHandle : 0;

  const filteredRecents = useMemo(() => {
    return recentConnections
      .map((rc) => ({
        rc,
        ...fuzzyMatch(filter, `${recentLabel(rc)} ${TYPE_LABELS[rc.type]}`),
      }))
      .filter(({ matches }) => matches)
      .sort((a, b) => b.score - a.score);
  }, [recentConnections, filter]);

  const filteredSaved = useMemo(() => {
    return connections
      .map((c) => ({
        c,
        ...fuzzyMatch(filter, `${c.name} ${c.id} ${connectionDetail(c)}`),
      }))
      .filter(({ matches }) => matches)
      .sort((a, b) => b.score - a.score);
  }, [connections, filter]);

  const reconnect = (rc: RecentConnection) => {
    switch (rc.type) {
      case "sftp":
        safe(commands.mountSftp(ph, rc.host));
        break;
      case "s3":
        break; // S3 is never recorded (keys aren't persisted); defensive.
      default:
        safe(commands.connectTarget(ph, rc, rc.open_in ?? "window"));
    }
  };

  const onSelect = (value: string) => {
    if (pendingDelete !== null) return;
    if (value.startsWith("recent:")) {
      const rc = filteredRecents[parseInt(value.slice(7), 10)]?.rc;
      if (rc) reconnect(rc);
    } else if (value.startsWith("saved:")) {
      safe(commands.connectProfile(ph, value.slice(6)));
    }
  };

  const requestDelete = (value: string, e?: React.MouseEvent) => {
    e?.stopPropagation();
    e?.preventDefault();
    setPendingDelete(value);
  };

  // Replaces this palette with the matching connect/mount dialog, prefilled.
  // Saved profiles open in edit mode (submit updates in place); recents just
  // prefill, letting the user tweak the target before connecting.
  const requestEdit = (value: string, e?: React.MouseEvent) => {
    e?.stopPropagation();
    e?.preventDefault();
    const modalPane = typeof paneHandle === "number" ? paneHandle : null;
    if (value.startsWith("recent:")) {
      const rc = filteredRecents[parseInt(value.slice(7), 10)]?.rc;
      if (rc) safe(commands.editRecentConnection(modalPane, rc));
    } else if (value.startsWith("saved:")) {
      safe(commands.editConnection(modalPane, value.slice(6)));
    }
  };

  const reopen = () =>
    safeSilent(
      commands.dialog(
        "quick_connect",
        typeof paneHandle === "number" ? paneHandle : null,
      ),
    );

  const confirmDelete = async (value: string) => {
    setPendingDelete(null);
    if (value.startsWith("recent:")) {
      const rc = filteredRecents[parseInt(value.slice(7), 10)]?.rc;
      if (rc) await safeSilent(commands.forgetRecentConnection(rc));
    } else if (value.startsWith("saved:")) {
      await safeSilent(commands.cmdDeleteConnection(value.slice(6)));
    }
    await reopen();
  };

  const cancelDelete = () => setPendingDelete(null);

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

    if (e.key === "Delete" || e.key === "F4") {
      const el = document.querySelector('[cmdk-item][data-selected="true"]');
      const value = el?.getAttribute("data-value");
      if (value) {
        e.preventDefault();
        if (e.key === "Delete") requestDelete(value);
        else requestEdit(value);
      }
    }
  };

  const confirmRow = (label: string) => (
    <div className={styles.confirmRow}>
      <span>{label}</span>
      <span className={styles.confirmActions}>
        <button
          className={styles.confirmYes}
          onClick={(e) => {
            e.stopPropagation();
            confirmDelete(pendingDelete!);
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
  );

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
            {connections.length === 0 && recentConnections.length === 0
              ? "No saved connections. Use the connect or mount dialogs to save one."
              : "No matching connections."}
          </Command.Empty>

          {filteredRecents.length > 0 && (
            <Command.Group heading="Recent">
              {filteredRecents.map(({ rc }, i) => {
                const value = `recent:${i}`;
                return (
                  <Command.Item key={value} value={value} onSelect={onSelect}>
                    {pendingDelete === value ? (
                      confirmRow("Forget this connection?")
                    ) : (
                      <>
                        <div className={styles.itemContent}>
                          <span className={styles.name}>
                            <Highlight
                              text={recentLabel(rc)}
                              filter={filter}
                              highlightClass={styles.highlight}
                            />
                          </span>
                          <span className={styles.path}>
                            {recentSubtitle(rc)}
                          </span>
                        </div>
                        <button
                          className={styles.editBtn}
                          onClick={(e) => requestEdit(value, e)}
                          title="Edit before connecting (F4)"
                          tabIndex={-1}
                        >
                          &#9998;
                        </button>
                        <button
                          className={styles.deleteBtn}
                          onClick={(e) => requestDelete(value, e)}
                          title="Forget connection"
                          tabIndex={-1}
                        >
                          &times;
                        </button>
                      </>
                    )}
                  </Command.Item>
                );
              })}
            </Command.Group>
          )}

          {filteredSaved.length > 0 && (
            <Command.Group
              heading={filteredRecents.length > 0 ? "Saved" : undefined}
            >
              {filteredSaved.map(({ c }) => {
                const value = `saved:${c.id}`;
                return (
                  <Command.Item key={value} value={value} onSelect={onSelect}>
                    {pendingDelete === value ? (
                      confirmRow("Remove connection?")
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
                          className={styles.editBtn}
                          onClick={(e) => requestEdit(value, e)}
                          title="Edit connection (F4)"
                          tabIndex={-1}
                        >
                          &#9998;
                        </button>
                        <button
                          className={styles.deleteBtn}
                          onClick={(e) => requestDelete(value, e)}
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
            </Command.Group>
          )}
        </Command.List>
      </Palette>
    </Dialog.Content>
  );
}
