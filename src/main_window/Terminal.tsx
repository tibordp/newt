import { useEffect, useRef, useContext } from "react";
import { Terminal as XTermJSTerminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import {
  TerminalData,
  registerTerminalDataHandler,
  safeCommandSilent,
} from "../lib/ipc";
import "@xterm/xterm/css/xterm.css";

export default function Terminal({ handle, active }: { handle: number; active: boolean }) {
  const terminalRef = useRef<XTermJSTerminal>(null);
  const ref = useRef<HTMLDivElement>(null);
  const termDataContext = useContext(TerminalData);

  useEffect(() => {
    const term = new XTermJSTerminal({
      scrollback: 1000,
      fontFamily: 'Menlo, Monaco, "Courier New", monospace', // Default Mac VS Code fonts
      fontSize: 12,
      lineHeight: 1.2,
      fontWeight: "normal",
      fontWeightBold: "bold",
      cursorStyle: "bar",     // VS Code uses a vertical bar by default
      cursorBlink: true,      // Makes the terminal feel active
      cursorWidth: 2,
      allowTransparency: true,
      allowProposedApi: true,

      theme: {
          background: "#ffffff",
          foreground: "#3b3b3b",       // Slightly softer than harsh black
          cursor: "#3b3b3b",
          selectionBackground: "#ADD6FF",

          // Standard Colors
          black: "#000000",
          red: "#a1260d",              // Deeper, more readable red
          green: "#107c41",            // Microsoft's modern, accessible green
          yellow: "#82660b",           // Muted yellow for better white-background contrast
          blue: "#0050a4",
          magenta: "#9e1c72",
          cyan: "#007185",
          white: "#5b5b5b",

          // Bright Colors (Often used for bold text in the terminal)
          brightBlack: "#666666",
          brightRed: "#cd3131",
          brightGreen: "#14ce14",
          brightYellow: "#b5ba00",
          brightBlue: "#0451a5",
          brightMagenta: "#bc05bc",
          brightCyan: "#0598bc",
          brightWhite: "#a5a5a5",
      },

    });
    term.open(ref.current!);
    terminalRef.current = term;

    const unregister = registerTerminalDataHandler(
      termDataContext,
      handle,
      (data) => {
        // @ts-ignore
        term.write(data);
      }
    );

    const onUserInput = (data) => {
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
    fitAddon.fit();
    const resizeObserver = new ResizeObserver(() => {
      fitAddon.fit();
    });
    resizeObserver.observe(ref.current!);

    return () => {
      terminalRef.current = null;
      unregister();
      term.dispose();
      if (ref.current) {
        resizeObserver.disconnect();
      }
    };
  }, []);

  useEffect(() => {
    if (active) {
      terminalRef.current?.focus();
    } else {
      terminalRef.current?.blur();
    }
  }, [active, handle]);

  return (
    <div className="terminal-container" >
      <div className="terminal" ref={ref} tabIndex={-1} onFocus={() => safeCommandSilent("terminal_focus", { handle })} />
    </div>
  );
}
