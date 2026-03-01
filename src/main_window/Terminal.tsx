import { useEffect, useRef, useContext } from "react";
import { Terminal as XTermJSTerminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import {
  TerminalData,
  registerTerminalDataHandler,
  safeCommandSilent,
} from "../lib/ipc";
import "@xterm/xterm/css/xterm.css";
import styles from "./Terminal.module.scss";

import type { ITheme } from "@xterm/xterm";

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
}: {
  handle: number;
  active: boolean;
  visible: boolean;
}) {
  const terminalRef = useRef<XTermJSTerminal>(null);
  const fitAddonRef = useRef<FitAddon>(null);
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const ref = useRef<HTMLDivElement>(null);
  const termDataContext = useContext(TerminalData);

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
      // Ctrl+` — toggle terminal panel
      if (e.ctrlKey && e.key === "`") return false;
      // Ctrl+Shift+` — new terminal (Shift+` produces ~)
      if (e.ctrlKey && e.shiftKey && e.key === "~") return false;
      // Ctrl+PageDown / Ctrl+PageUp — cycle tabs
      if (e.ctrlKey && (e.key === "PageDown" || e.key === "PageUp"))
        return false;
      return true;
    });

    const unregister = registerTerminalDataHandler(
      termDataContext,
      handle,
      (data) => {
        // @ts-expect-error data is number[] but xterm accepts it
        term.write(data);
      },
    );

    const onUserInput = (data: string) => {
      const binaryData = new TextEncoder().encode(data);
      safeCommandSilent("terminal_write", { handle, data: [...binaryData] });
    };

    term.onBinary(onUserInput);
    term.onData(onUserInput);
    term.onResize((size) => {
      safeCommandSilent("terminal_resize", {
        handle,
        rows: size.rows,
        cols: size.cols,
      });
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    fitAddonRef.current = fitAddon;
    fitAddon.fit();
    const resizeObserver = new ResizeObserver(() => {
      if (visibleRef.current) {
        fitAddon.fit();
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
      const raf = requestAnimationFrame(() => {
        fitAddonRef.current?.fit();
      });
      return () => cancelAnimationFrame(raf);
    }
  }, [visible]);

  useEffect(() => {
    if (active) {
      terminalRef.current?.focus();
    } else {
      terminalRef.current?.blur();
    }
  }, [active, handle]);

  return (
    <div className={styles.container}>
      <div
        className={styles.terminal}
        ref={ref}
        tabIndex={-1}
        onFocus={() => safeCommandSilent("terminal_focus", { handle })}
      />
    </div>
  );
}
