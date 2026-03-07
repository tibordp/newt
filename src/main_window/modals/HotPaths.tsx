import { useEffect, useMemo, useState, ReactElement } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Command } from "cmdk";
import { invoke } from "@tauri-apps/api/core";
import { safeCommand } from "../../lib/ipc";
import { MainWindowState } from "../types";
import { VfsPath } from "../../lib/types";
import styles from "./HotPaths.module.scss";

type HotPathCategory =
  | "UserBookmark"
  | "StandardFolder"
  | "Bookmark"
  | "Mount"
  | "RecentFolder";

type HotPathEntry = {
  path: VfsPath;
  name: string | null;
  category: HotPathCategory;
};

const CATEGORY_LABELS: Record<HotPathCategory, string> = {
  UserBookmark: "Bookmarks",
  StandardFolder: "Standard Folders",
  Bookmark: "System Bookmarks",
  Mount: "Volumes",
  RecentFolder: "Recent Folders",
};

const CATEGORY_ORDER: HotPathCategory[] = [
  "UserBookmark",
  "StandardFolder",
  "Bookmark",
  "Mount",
  "RecentFolder",
];

const preventAutoFocus = (e: Event) => e.preventDefault();

function Highlight({ text, filter }: { text: string; filter: string }) {
  let a = 0;
  let b = 0;
  let key = 0;
  const parts: ReactElement[] = [];

  while (a < filter.length && b < text.length) {
    if (filter[a].toLowerCase() === text[b].toLowerCase()) {
      parts.push(
        <span key={key++} className={styles.highlight}>
          {text[b]}
        </span>,
      );
      a++;
      b++;
    } else {
      parts.push(<span key={key++}>{text[b]}</span>);
      b++;
    }
  }

  if (b < text.length) {
    parts.push(<span key={key}>{text.slice(b)}</span>);
  }

  return <span>{parts}</span>;
}

function displayPath(entry: HotPathEntry): string {
  return entry.path.path;
}

function searchableText(entry: HotPathEntry): string {
  const parts = [entry.path.path];
  if (entry.name) parts.push(entry.name);
  return parts.join(" ");
}

function fuzzyMatch(
  filter: string,
  text: string,
): { matches: boolean; score: number } {
  let a = 0;
  let b = 0;
  let consecutive = 0;
  let maxConsecutive = 0;

  while (a < filter.length && b < text.length) {
    if (filter[a].toLowerCase() === text[b].toLowerCase()) {
      consecutive++;
      a++;
      b++;
    } else {
      maxConsecutive = Math.max(maxConsecutive, consecutive);
      consecutive = 0;
      b++;
    }
  }

  return {
    matches: a === filter.length,
    score: Math.max(maxConsecutive, consecutive),
  };
}

export default function HotPaths({ state }: { state: MainWindowState | null }) {
  const [filter, setFilter] = useState("");
  const [entries, setEntries] = useState<HotPathEntry[]>([]);
  const [loading, setLoading] = useState(true);
  // Path string of the bookmark pending deletion confirmation, or null
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  const paneHandle =
    state?.display_options.panes_focused && state?.display_options.active_pane;

  // Fetch hot paths on mount
  useEffect(() => {
    invoke<HotPathEntry[]>("get_hot_paths")
      .then(setEntries)
      .catch(console.error)
      .finally(() => setLoading(false));
  }, []);

  // Filter and group entries
  const grouped = useMemo(() => {
    const filtered = entries
      .map((entry) => {
        const text = searchableText(entry);
        const result = fuzzyMatch(filter, text);
        return { entry, ...result };
      })
      .filter(({ matches }) => matches);

    filtered.sort((a, b) => b.score - a.score);

    // Group by category in display order
    const groups: { category: HotPathCategory; items: HotPathEntry[] }[] = [];
    for (const cat of CATEGORY_ORDER) {
      const items = filtered
        .filter(({ entry }) => entry.category === cat)
        .map(({ entry }) => entry);
      if (items.length > 0) {
        groups.push({ category: cat, items });
      }
    }

    return groups;
  }, [entries, filter]);

  const onSelect = (value: string) => {
    // If confirming a delete, don't navigate
    if (pendingDelete !== null) return;

    const [cat, idxStr] = value.split(":");
    const group = grouped.find((g) => g.category === cat);
    if (!group) return;
    const entry = group.items[parseInt(idxStr, 10)];
    if (!entry) return;

    safeCommand("navigate", {
      paneHandle: paneHandle || 0,
      path: entry.path.path,
      exact: false,
    });
  };

  const requestDelete = (entry: HotPathEntry, e?: React.MouseEvent) => {
    e?.stopPropagation();
    e?.preventDefault();
    setPendingDelete(entry.path.path);
  };

  const confirmDelete = (path: string) => {
    invoke("remove_bookmark", { path })
      .then(() => {
        setEntries((prev) =>
          prev.filter(
            (e) => !(e.path.path === path && e.category === "UserBookmark"),
          ),
        );
        setPendingDelete(null);
      })
      .catch(console.error);
  };

  const cancelDelete = () => {
    setPendingDelete(null);
  };

  const getSelectedBookmarkEntry = (): HotPathEntry | null => {
    const selected = document.querySelector(
      '[cmdk-item][data-selected="true"]',
    );
    if (!selected) return null;
    const value = selected.getAttribute("data-value");
    if (!value) return null;
    const [cat, idxStr] = value.split(":");
    if (cat !== "UserBookmark") return null;
    const group = grouped.find((g) => g.category === cat);
    if (!group) return null;
    return group.items[parseInt(idxStr, 10)] ?? null;
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (pendingDelete !== null) {
      // While confirming: Enter/Y confirms, N cancels
      // (Escape is handled by onEscapeKeyDown on Dialog.Content)
      if (e.key === "Enter" || e.key === "y" || e.key === "Y") {
        e.preventDefault();
        e.stopPropagation();
        confirmDelete(pendingDelete);
      } else if (e.key === "n" || e.key === "N") {
        e.preventDefault();
        e.stopPropagation();
        cancelDelete();
      } else {
        // Swallow all other keys during confirmation
        e.preventDefault();
        e.stopPropagation();
      }
      return;
    }

    if (e.key === "Delete") {
      const entry = getSelectedBookmarkEntry();
      if (!entry) return;
      e.preventDefault();
      requestDelete(entry);
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
      <Dialog.Title className="sr-only">Hot Paths</Dialog.Title>
      <Command shouldFilter={false} onKeyDown={onKeyDown}>
        <div className={styles.header}>
          <Command.Input
            value={filter}
            onValueChange={setFilter}
            placeholder="Search paths..."
          />
        </div>
        <Command.List>
          {loading && <Command.Loading>Loading...</Command.Loading>}
          <Command.Empty>No paths found</Command.Empty>
          {grouped.map(({ category, items }) => (
            <Command.Group key={category} heading={CATEGORY_LABELS[category]}>
              {items.map((entry, i) => {
                const isConfirming =
                  pendingDelete === entry.path.path &&
                  entry.category === "UserBookmark";

                return (
                  <Command.Item
                    key={`${category}-${i}`}
                    value={`${category}:${i}`}
                    onSelect={onSelect}
                  >
                    {isConfirming ? (
                      <div className={styles.confirmRow}>
                        <span>Remove bookmark?</span>
                        <span className={styles.confirmActions}>
                          <button
                            className={styles.confirmYes}
                            onClick={(e) => {
                              e.stopPropagation();
                              confirmDelete(entry.path.path);
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
                          {entry.name ? (
                            <>
                              <span className={styles.name}>
                                <Highlight text={entry.name} filter={filter} />
                              </span>
                              <span className={styles.path}>
                                <Highlight
                                  text={displayPath(entry)}
                                  filter={filter}
                                />
                              </span>
                            </>
                          ) : (
                            <span className={styles.name}>
                              <Highlight
                                text={displayPath(entry)}
                                filter={filter}
                              />
                            </span>
                          )}
                        </div>
                        {entry.category === "UserBookmark" && (
                          <button
                            className={styles.deleteBtn}
                            onClick={(e) => requestDelete(entry, e)}
                            title="Remove bookmark"
                            tabIndex={-1}
                          >
                            &times;
                          </button>
                        )}
                      </>
                    )}
                  </Command.Item>
                );
              })}
            </Command.Group>
          ))}
        </Command.List>
      </Command>
    </Dialog.Content>
  );
}
