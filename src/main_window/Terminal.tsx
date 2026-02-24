import { useEffect, useRef, useContext } from "react";
import { Terminal as XTermJSTerminal } from "xterm";
import { FitAddon } from "xterm-addon-fit";
import {
  TerminalData,
  registerTerminalDataHandler,
  safeCommandSilent,
} from "../lib/ipc";
import "xterm/css/xterm.css";

export default function Terminal({ handle, active }: { handle: number; active: boolean }) {
  const terminalRef = useRef<XTermJSTerminal>(null);
  const ref = useRef<HTMLDivElement>(null);
  const termDataContext = useContext(TerminalData);

  useEffect(() => {
    const term = new XTermJSTerminal({
      scrollback: 1000,
      fontFamily: "monospace",
      fontSize: 13,
      cursorStyle: "block",
      allowTransparency: true,
      allowProposedApi: true,

      theme: {
        cursor: "#000000",
        background: "#ffffff",
        foreground: "#333333",
        selectionBackground: "#ADD6FF",
        black: "#000000",
        red: "#cd3131",
        green: "#00BC00",
        yellow: "#949800",
        blue: "#0451a5",
        magenta: "#bc05bc",
        cyan: "#0598bc",
        white: "#555555",
        brightBlack: "#666666",
        brightRed: "#cd3131",
        brightGreen: "#14CE14",
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
