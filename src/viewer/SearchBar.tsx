import React, { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import styles from "./Viewer.module.scss";
import type { VfsPath } from "./helpers";

interface SearchMatch {
  offset: number;
  length: number;
}

type SearchMode = "text" | "hex";

export interface SearchBarProps {
  open: boolean;
  onClose: () => void;
  vfsPath: VfsPath;
  fileSize: number;
  /** "text" for text viewer (search as UTF-8), "hex" for hex viewer */
  mode: SearchMode;
  /** Called when a match is found — viewer should scroll to this byte range */
  onMatch: (match: SearchMatch) => void;
  /** Called when search wraps or finds nothing */
  onNoMatch: () => void;
}

function parseHexBytes(input: string): Uint8Array | null {
  const cleaned = input.replace(/\s+/g, "");
  if (cleaned.length === 0 || cleaned.length % 2 !== 0) return null;
  if (!/^[0-9a-fA-F]+$/.test(cleaned)) return null;
  const bytes = new Uint8Array(cleaned.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(cleaned.substring(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

export function SearchBar({
  open,
  onClose,
  vfsPath,
  fileSize,
  mode,
  onMatch,
  onNoMatch,
}: SearchBarProps) {
  const [query, setQuery] = useState("");
  const [useRegex, setUseRegex] = useState(false);
  const [hexMode, setHexMode] = useState(false);
  const [searching, setSearching] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const lastMatchRef = useRef<SearchMatch | null>(null);
  const searchOffsetRef = useRef(0);

  useEffect(() => {
    if (open) {
      inputRef.current?.focus();
      inputRef.current?.select();
      setStatus(null);
    } else {
      lastMatchRef.current = null;
      searchOffsetRef.current = 0;
    }
  }, [open]);

  // Refocus input after search completes — disabling the input during
  // search moves focus to <body>, breaking keyboard handling.
  useEffect(() => {
    if (!searching && open) {
      inputRef.current?.focus();
    }
  }, [searching, open]);

  useEffect(() => {
    setHexMode(mode === "hex");
  }, [mode]);

  const doSearch = useCallback(
    async (fromOffset: number) => {
      if (!query.trim()) return;

      let pattern: { Literal: number[] } | { Regex: string };
      if (hexMode) {
        const bytes = parseHexBytes(query);
        if (!bytes) {
          setStatus("Invalid hex");
          return;
        }
        pattern = { Literal: Array.from(bytes) };
      } else if (useRegex) {
        pattern = { Regex: query };
      } else {
        pattern = { Literal: Array.from(new TextEncoder().encode(query)) };
      }

      setSearching(true);
      setStatus(null);

      try {
        const maxLength = fileSize > 0 ? fileSize : 1024 * 1024 * 1024;
        const result: SearchMatch | null = await invoke("find_in_viewer", {
          path: vfsPath,
          offset: fromOffset,
          pattern,
          maxLength,
        });

        if (result) {
          lastMatchRef.current = result;
          searchOffsetRef.current = result.offset + 1;
          onMatch(result);
          setStatus(null);
        } else if (fromOffset > 0) {
          const wrapResult: SearchMatch | null = await invoke(
            "find_in_viewer",
            {
              path: vfsPath,
              offset: 0,
              pattern,
              maxLength: fromOffset,
            },
          );
          if (wrapResult) {
            lastMatchRef.current = wrapResult;
            searchOffsetRef.current = wrapResult.offset + 1;
            onMatch(wrapResult);
            setStatus("Wrapped");
          } else {
            setStatus("Not found");
            onNoMatch();
          }
        } else {
          setStatus("Not found");
          onNoMatch();
        }
      } catch (e: any) {
        setStatus(e.toString());
      } finally {
        setSearching(false);
      }
    },
    [query, useRegex, hexMode, vfsPath, fileSize, onMatch, onNoMatch],
  );

  const findNext = useCallback(() => {
    doSearch(searchOffsetRef.current);
  }, [doSearch]);

  const findPrev = useCallback(() => {
    searchOffsetRef.current = 0;
    doSearch(0);
  }, [doSearch]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (e.shiftKey) {
          findPrev();
        } else {
          findNext();
        }
      }
    },
    [onClose, findNext, findPrev],
  );

  if (!open) return null;

  return (
    <div className={styles.searchBar} onKeyDown={handleKeyDown}>
      <input
        ref={inputRef}
        className={styles.searchInput}
        type="text"
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          setStatus(null);
          searchOffsetRef.current = 0;
        }}
        placeholder={
          hexMode
            ? "Hex bytes (e.g. 4D 5A)"
            : useRegex
              ? "Regex pattern"
              : "Search text..."
        }
        disabled={searching}
      />
      {mode === "hex" && (
        <button
          className={`${styles.searchToggle} ${hexMode ? styles.searchToggleActive : ""}`}
          onClick={() => {
            setHexMode(!hexMode);
            setUseRegex(false);
            setStatus(null);
          }}
          title="Search as hex bytes"
        >
          Hex
        </button>
      )}
      {!hexMode && (
        <button
          className={`${styles.searchToggle} ${useRegex ? styles.searchToggleActive : ""}`}
          onClick={() => {
            setUseRegex(!useRegex);
            setStatus(null);
          }}
          title="Use regular expression"
        >
          .*
        </button>
      )}
      <button
        className={styles.searchBtn}
        onClick={findNext}
        disabled={searching || !query.trim()}
        title="Find Next (Enter)"
      >
        {"\u25BC"}
      </button>
      <button
        className={styles.searchBtn}
        onClick={findPrev}
        disabled={searching || !query.trim()}
        title="Find from start (Shift+Enter)"
      >
        {"\u25B2"}
      </button>
      {status && <span className={styles.searchStatus}>{status}</span>}
      <button
        className={styles.searchBtn}
        onClick={onClose}
        title="Close (Escape)"
      >
        {"\u2715"}
      </button>
    </div>
  );
}
