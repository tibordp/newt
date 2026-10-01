import { useEffect, useRef, useContext, useState } from "react";
import { ContextMenu as CM } from "../lib/menus";
import { Terminal as XTermJSTerminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import {
  TerminalData,
  rawCommands,
  registerTerminalDataHandler,
  safeSilent,
} from "../lib/ipc";
import "@xterm/xterm/css/xterm.css";
import styles from "./Terminal.module.scss";

import type { ITheme } from "@xterm/xterm";
import { commands } from "../lib/bindings";
import { REFOCUS_EVENT } from "./types";
import { TerminalMenuContent } from "./TerminalMenus";

const lightTheme: ITheme = {
  background: "#ffffff",
  foreground: "#3b3b3b",
  cursor: "#3b3b3b",
  selectionBackground: "#ADD6FF",
  black: "#000000",
  red: "#a1260d",
  green: "#107c41",
  yellow: "#82660b",
  blue: "#0050a4",
  magenta: "#9e1c72",
  cyan: "#007185",
  white: "#5b5b5b",
  brightBlack: "#666666",
  brightRed: "#cd3131",
  brightGreen: "#14ce14",
  brightYellow: "#b5ba00",
  brightBlue: "#0451a5",
  brightMagenta: "#bc05bc",
  brightCyan: "#0598bc",
  brightWhite: "#a5a5a5",
};

const darkTheme: ITheme = {
  background: "#1e1e1e",
  foreground: "#cccccc",
  cursor: "#cccccc",
  selectionBackground: "#264f78",
  black: "#1e1e1e",
  red: "#f44747",
  green: "#6a9955",
  yellow: "#d7ba7d",
  blue: "#569cd6",
  magenta: "#c586c0",
  cyan: "#4ec9b0",
  white: "#d4d4d4",
  brightBlack: "#808080",
  brightRed: "#f44747",
  brightGreen: "#6a9955",
  brightYellow: "#d7ba7d",
  brightBlue: "#569cd6",
  brightMagenta: "#c586c0",
  brightCyan: "#4ec9b0",
  brightWhite: "#e8e8e8",
};

function getPreferredTheme(): ITheme {
  const dataTheme = document.documentElement.dataset.theme;
  if (dataTheme === "dark") return darkTheme;
  if (dataTheme === "light") return lightTheme;
  return window.matchMedia("(prefers-color-scheme: dark)").matches
    ? darkTheme
    : lightTheme;
}

export default function Terminal({
  handle,
  active,
  visible,
  modalOpen,
  defunct,
  maximized,
}: {
  handle: number;
  active: boolean;
  visible: boolean;
  modalOpen: boolean;
  defunct: boolean;
  maximized: boolean;
}) {
  const [hasSelection, setHasSelection] = useState(false);
  const terminalRef = useRef<XTermJSTerminal>(null);
  const fitAddonRef = useRef<FitAddon>(null);
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const defunctRef = useRef(defunct);
  defunctRef.current = defunct;
  const ref = useRef<HTMLDivElement>(null);
  const termDataContext = useContext(TerminalData);

  // A hidden panel is collapsed to zero size rather than removed, and fitting
  // it would shrink the PTY to a single row under whatever is running.
  const fitIfSized = () => {
    const el = ref.current;
    if (el && el.clientWidth > 0 && el.clientHeight > 0) {
      fitAddonRef.current?.fit();
    }
  };

  useEffect(() => {
    const term = new XTermJSTerminal({
      scrollback: 1000,
      fontFamily: 'Menlo, Monaco, "Courier New", monospace',
      fontSize: 12,
      lineHeight: 1.2,
      fontWeight: "normal",
      fontWeightBold: "bold",
      cursorStyle: "bar",
      cursorBlink: true,
      cursorWidth: 2,
      allowTransparency: true,
      allowProposedApi: true,
      theme: getPreferredTheme(),
    });
    term.open(ref.current!);
    terminalRef.current = term;

    // Let panel-level shortcuts bubble through xterm
    term.attachCustomKeyEventHandler((e: KeyboardEvent) => {
      if (e.type !== "keydown") return true;
      // Ctrl+Shift+C or Cmd+C — copy selection to clipboard
      if (
        (e.key === "c" || e.key === "C") &&
        (e.metaKey || (e.ctrlKey && e.shiftKey))
      ) {
        const sel = term.getSelection();
        if (sel) navigator.clipboard.writeText(sel);
        return false;
      }
      // Defunct terminal: Enter closes it
      if (defunctRef.current && e.key === "Enter") {
        safeSilent(commands.closeTerminal(handle));
        return false;
      }
      // Ctrl+` — toggle terminal panel
      if (e.ctrlKey && e.key === "`") return false;
      // Ctrl+Shift+` — new terminal (Shift+` produces ~)
      if (e.ctrlKey && e.shiftKey && e.key === "~") return false;
      // Ctrl+PageDown / Ctrl+PageUp — cycle tabs
      if (e.ctrlKey && (e.key === "PageDown" || e.key === "PageUp"))
        return false;
      // Alt+Up / Alt+Down — focus panes / terminal
      if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown"))
        return false;
      // Mod+F11 — toggle maximized layout
      if (
        e.key === "F11" &&
        (navigator.platform.startsWith("Mac") ? e.metaKey : e.ctrlKey)
      )
        return false;
      return true;
    });

    const unregister = registerTerminalDataHandler(
      termDataContext,
      handle,
      (data) => {
        term.write(data);
      },
    );

    // Serialize keystroke dispatch on a single promise chain so concurrent
    // Tauri command tasks can't race to enqueue on the RPC outbox out of
    // order. The backend's `terminal_write` returns as soon as the bytes are
    // either written to the local PTY or enqueued on the high-priority RPC
    // lane (no wire RTT), so chaining doesn't cost pipelining.
    let writeChain: Promise<unknown> = Promise.resolve();
    const onUserInput = (data: string) => {
      const binaryData = new TextEncoder().encode(data);
      writeChain = writeChain.then(() =>
        safeSilent(rawCommands.terminalWrite(handle, binaryData)),
      );
    };

    term.onBinary(onUserInput);
    term.onData(onUserInput);
    term.onResize((size) => {
      safeSilent(commands.terminalResize(handle, size.rows, size.cols));
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    fitAddonRef.current = fitAddon;
    fitIfSized();
    const resizeObserver = new ResizeObserver(() => {
      if (visibleRef.current) {
        fitIfSized();
      }
    });
    resizeObserver.observe(ref.current!);

    const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
    const onThemeChange = () => {
      term.options.theme = getPreferredTheme();
    };
    mediaQuery.addEventListener("change", onThemeChange);

    return () => {
      terminalRef.current = null;
      fitAddonRef.current = null;
      unregister();
      term.dispose();
      mediaQuery.removeEventListener("change", onThemeChange);
      if (ref.current) {
        resizeObserver.disconnect();
      }
    };
  }, []);

  useEffect(() => {
    if (visible) {
      // Defer fit() so the browser has reflowed the now-visible container
      const raf = requestAnimationFrame(fitIfSized);
      return () => cancelAnimationFrame(raf);
    }
  }, [visible]);

  useEffect(() => {
    if (active && !modalOpen) {
      terminalRef.current?.focus();
    } else if (!active) {
      terminalRef.current?.blur();
    }
  }, [active, handle, modalOpen]);

  useEffect(() => {
    if (!active || modalOpen) return;
    const refocus = () => terminalRef.current?.focus();
    window.addEventListener(REFOCUS_EVENT, refocus);
    return () => window.removeEventListener(REFOCUS_EVENT, refocus);
  }, [active, modalOpen]);

  return (
    <CM.Root
      onOpenChange={(open) => {
        if (open) setHasSelection(!!terminalRef.current?.hasSelection());
      }}
    >
      <CM.Trigger
        asChild
        onContextMenu={(e) => {
          // A program reporting the mouse (tmux, vim with mouse=a) gets the
          // right-click; Shift+right-click reaches the menu regardless, as
          // Shift+drag selects regardless. Preventing the default here also
          // keeps Radix from opening.
          const tracking = terminalRef.current?.modes.mouseTrackingMode;
          if (tracking && tracking !== "none" && !e.shiftKey)
            e.preventDefault();
        }}
      >
        <div className={styles.container}>
          <div
            className={styles.terminal}
            ref={ref}
            tabIndex={-1}
            onFocus={() => safeSilent(commands.terminalFocus(handle))}
          />
        </div>
      </CM.Trigger>
      <TerminalMenuContent
        handle={handle}
        hasSelection={hasSelection}
        maximized={maximized}
        onCopy={() => {
          const sel = terminalRef.current?.getSelection();
          if (sel) navigator.clipboard.writeText(sel);
        }}
        onPaste={async () => {
          const text = await commands.readClipboardText();
          if (text.status === "ok") terminalRef.current?.paste(text.data);
        }}
        onSelectAll={() => terminalRef.current?.selectAll()}
        onClear={() => terminalRef.current?.clear()}
      />
    </CM.Root>
  );
}
