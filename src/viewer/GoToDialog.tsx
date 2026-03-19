import React, { useCallback, useEffect, useRef, useState } from "react";

import styles from "./Viewer.module.scss";

interface GoToBarProps {
  open: boolean;
  onClose: () => void;
  label: string;
  placeholder: string;
  onSubmit: (value: string) => void;
}

export function GoToBar({
  open,
  onClose,
  label,
  placeholder,
  onSubmit,
}: GoToBarProps) {
  const [value, setValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setValue("");
      inputRef.current?.focus();
    }
  }, [open]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (value.trim()) {
          onSubmit(value.trim());
          onClose();
        }
      }
    },
    [onClose, onSubmit, value],
  );

  if (!open) return null;

  return (
    <div className={styles.searchBar} onKeyDown={handleKeyDown}>
      <span className={styles.searchStatus}>{label}:</span>
      <input
        ref={inputRef}
        className={styles.searchInput}
        type="text"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        placeholder={placeholder}
        autoFocus
      />
      <button
        className={styles.searchBtn}
        onClick={() => {
          if (value.trim()) {
            onSubmit(value.trim());
            onClose();
          }
        }}
        disabled={!value.trim()}
        title="Go (Enter)"
      >
        Go
      </button>
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
