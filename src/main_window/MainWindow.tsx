import {
  useState,
  useEffect,
  useRef,
  useMemo,
  useLayoutEffect,
  useContext,
} from "react";
import { invoke } from "@tauri-apps/api/tauri";

import { Allotment } from "allotment";
import "./MainWindow.css";
import "allotment/dist/style.css";
import { confirm } from "@tauri-apps/api/dialog";

import iconMapping from "../assets/mapping.json";
import { ViewportList, ViewportListRef } from "../lib/viewPortList";
import { Profiler } from "react";
import { enablePatches } from "immer";

import {
  TerminalData,
  registerTerminalDataHandler,
  safeCommand,
  safeCommandSilent,
  useRemoteState,
  useTerminalData,
} from "../lib/ipc";
import { Terminal as XTermJSTerminal } from "xterm";
import { FitAddon } from "xterm-addon-fit";

import ReactModal from "react-modal";

import "xterm/css/xterm.css";
import "@fontsource-variable/roboto-mono";
import { ModalContent, ModalState } from "./modals/ModalContent";
import { P } from "@tauri-apps/api/event-30ea0228";

enablePatches();

type File = {
  name: string;
  size?: number;
  is_dir: boolean;
  is_symlink: boolean;
  is_hidden: boolean;
  mode: number;
  modified: number;
  accessed: number;
  created: number;
};

function FileName({ focused, filter, info }) {
  const { name, is_dir, is_symlink, is_hidden } = info;

  const icon =
    iconMapping.light.fileNames[name] ||
    iconMapping.light.fileExtensions[name.substr(name.indexOf(".") + 1)] ||
    iconMapping.light.file;

  const { fontCharacter, fontColor } = iconMapping.iconDefinitions[icon];
  const ch = String.fromCodePoint(parseInt(fontCharacter, 16));

  const nameElement = (
    <>
      {(!focused || filter === null) && <>{name}</>}
      {focused && filter !== null && (
        <>
          <span className="filter-head">{name.substr(0, filter.length)}</span>
          <span className="filter-tail">{name.substr(filter.length)}</span>
        </>
      )}
    </>
  );

  const iconElement = is_dir ? (
    <div className="file-icon folder" />
  ) : (
    <div className="file-icon" style={{ color: fontColor }}>
      {ch}
    </div>
  );

  return (
    <div
      className={`filename ${is_hidden ? "hidden-file" : ""} ${
        is_symlink ? "symlink" : ""
      }`}
    >
      {iconElement}
      <div className={focused ? "filename-part focused" : "filename-part"}>
        {nameElement}
      </div>
    </div>
  );
}

type ColumnDef = {
  align: "left" | "right" | "center";
  initialWidth: number;
  subcolumns?: SubcolumnDef[];
  key: string;
  render: (info: File, paneProps: PaneState) => JSX.Element;
};

type SubcolumnDef = {
  name: string;
  sortKey?: string;
  style?: React.CSSProperties;
};

const columns: ColumnDef[] = [
  {
    align: "left",
    key: "name",
    subcolumns: [
      {
        sortKey: "name",
        name: "Name",
        style: {
          flexBasis: "60px",
        },
      },
      {
        sortKey: "extension",
        name: "Ext",
      },
    ],
    render: (info, { filter, focused, active }) => (
      <FileName
        filter={filter}
        focused={active && focused == info.name}
        info={info}
      />
    ),
    initialWidth: 250,
  },
  {
    align: "right",
    key: "size",
    initialWidth: 100,
    subcolumns: [
      {
        name: "Size",
        sortKey: "size",
      },
    ],
    render: (info) => (
      <>
        {info.size !== null
          ? info.size.toLocaleString()
          : info.is_dir
          ? "DIR"
          : "???"}
      </>
    ),
  },
  {
    align: "right",
    initialWidth: 70,
    key: "modified_date",
    subcolumns: [
      {
        name: "Date",
        sortKey: "modified",
      },
    ],
    render: (info) => <>{new Date(info.modified).toLocaleDateString()}</>,
  },
  {
    align: "right",
    initialWidth: 70,
    key: "modified_time",
    subcolumns: [
      {
        name: "Time",
        sortKey: "modified",
      },
    ],
    render: (info) => <>{new Date(info.modified).toLocaleTimeString()}</>,
  },
  {
    align: "left",
    initialWidth: 70,
    key: "mode",
    subcolumns: [
      {
        name: "Mode",
      },
    ],
    render: (info) => <>{info.mode}</>,
  },
];

type Sorting = {
  key: string;
  asc: boolean;
};

type PaneState = {
  path: string;
  pending_path?: string;
  sorting: Sorting;
  files: File[];
  focused?: string;
  selected: string[];
  active: boolean;
  filter?: string;
};

type DisplayOptions = {
  show_hidden: boolean;
  active_pane: number;
  panes_focused: boolean;
  active_terminal?: string;
};

type Terminal = {
  handle: string;
};

type MainWindowState = {
  panes: PaneState[];
  terminals: Terminal[];
  display_options: DisplayOptions;
  modal?: ModalState;
};

type ColumnHeaderProps = {
  widthPrefix: string;
  column: ColumnDef;
  sorting: Sorting;
  onSort: (key: string, asc: boolean) => void;
};

function ColumnHeader({
  widthPrefix,
  column,
  sorting,
  onSort,
}: ColumnHeaderProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [startOffset, setStartOffset] = useState(null);

  const onmousedown = (e) => {
    e.preventDefault();
    setStartOffset(ref.current.offsetWidth - e.clientX);
  };

  const onmouseup = (e) => {
    if (startOffset !== null) {
      e.preventDefault();
      setStartOffset(null);
    }
  };

  const onmousemove = (e) => {
    if (startOffset !== null && startOffset + e.clientX > 10) {
      e.preventDefault();
      const root = document.querySelector(":root");
      // @ts-ignore
      root.style.setProperty(
        `--${widthPrefix}-${column.key}`,
        `${startOffset + e.clientX}px`
      );
    }
  };

  useEffect(() => {
    document.addEventListener("mouseup", onmouseup);
    document.addEventListener("mousemove", onmousemove);

    return () => {
      document.removeEventListener("mouseup", onmouseup);
      document.removeEventListener("mousemove", onmousemove);
    };
  }, [startOffset]);

  useEffect(() => {
    const root = document.querySelector(":root");
    // @ts-ignore
    root.style.setProperty(
      `--${widthPrefix}-${column.key}`,
      `${column.initialWidth}px`
    );
  }, []);

  const defaultSubcolStyle = {
    flexGrow: 1,
    flexShrink: 1,
  };

  return (
    <>
      <div
        ref={ref}
        className={`column`}
        style={{
          width: `var(--${widthPrefix}-${column.key})`,
          textAlign: column.align,
        }}
      >
        {column.subcolumns.map((subcol, i) => (
          <div
            ref={ref}
            className={`subcolumn ${subcol.sortKey ? "sortable" : ""}`}
            onClick={(e: React.MouseEvent) => {
              e.stopPropagation();
              if (subcol.sortKey) {
                onSort(
                  subcol.sortKey,
                  sorting.key != subcol.sortKey || !sorting.asc
                );
              }
            }}
            style={subcol.style || defaultSubcolStyle}
          >
            {column.align == "right" && (
              <>
                {sorting.key == subcol.sortKey && sorting.asc && (
                  <span className="sorting-indicator">▲ </span>
                )}
                {sorting.key == subcol.sortKey && !sorting.asc && (
                  <span className="sorting-indicator">▼ </span>
                )}
              </>
            )}
            {subcol.name}
            {column.align == "left" && (
              <>
                {sorting.key == subcol.sortKey && sorting.asc && (
                  <span className="sorting-indicator"> ▲</span>
                )}
                {sorting.key == subcol.sortKey && !sorting.asc && (
                  <span className="sorting-indicator"> ▼</span>
                )}
              </>
            )}
          </div>
        ))}
      </div>
      <div className="column-grip" onMouseDown={onmousedown}></div>
    </>
  );
}

function Pane(props: PaneState & { paneHandle: number; active: boolean }) {
  const {
    paneHandle,
    active,
    filter,
    path,
    files,
    selected,
    sorting,
    focused,
    pending_path,
  } = props;
  const command = (cmd: string, args: object = {}, also_when_busy = false) => {
    if (also_when_busy || !pending_path) {
      safeCommand(cmd, { paneHandle, ...args });
    }
  };

  const [showSpinner, setShowSpinner] = useState(false);

  useEffect(() => {
    let timeout = null;
    if (pending_path) {
      // 200 ms of grace period before showing the loading screen to
      // appear smoother.
      timeout = setTimeout(() => setShowSpinner(true), 200);
    } else {
      setShowSpinner(false);
    }
    return () => {
      clearTimeout(timeout);
    };
  }, [pending_path]);

  // Without this lookup, rendering suddenly becomes O(n^2), which is very slow
  // when someone Ctrl+A's a directory with 1000+ files.
  const selectedLookup = useMemo(() => {
    return new Set(selected);
  }, [selected]);

  const [
    bytes,
    fileCount,
    dirCount,
    selectedBytes,
    selectedFileCount,
    selectedDirCount,
  ] = useMemo(() => {
    let bytes = 0;
    let fileCount = 0;
    let dirCount = 0;
    let selectedBytes = 0;
    let selectedFileCount = 0;
    let selectedDirCount = 0;

    for (const f of files) {
      if (f.is_dir) {
        dirCount++;
      } else {
        fileCount++;
        bytes += f.size;
      }
      if (selectedLookup.has(f.name)) {
        if (f.is_dir) {
          selectedDirCount++;
        } else {
          selectedFileCount++;
          selectedBytes += f.size;
        }
      }
    }
    return [
      bytes,
      fileCount,
      dirCount,
      selectedBytes,
      selectedFileCount,
      selectedDirCount,
    ];
  }, [files, selectedLookup]);

  const focusedIndex = useMemo(() => {
    if (files) {
      return files.findIndex((f) => f.name === focused);
    }
    return -1;
  }, [files, focused]);

  const containerRef = useRef<HTMLUListElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const viewPortRef = useRef<ViewportListRef>(null);
  const tableHeaderRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (active && files && viewPortRef.current) {
      const containerHeight = containerRef.current!.offsetHeight;
      const pos = viewPortRef.current.getScrollPosition();
      if (
        focusedIndex < pos.index ||
        (focusedIndex == pos.index && pos.offset > 0)
      ) {
        viewPortRef.current.scrollToIndex({
          index: focusedIndex,
          delay: 0,
          alignToTop: true,
          prerender: Math.ceil(containerHeight / 20),
        });
      } else if (focusedIndex >= pos.index + Math.floor(containerHeight / 20)) {
        viewPortRef.current.scrollToIndex({
          index: focusedIndex,
          delay: 0,
          alignToTop: false,
          prerender: Math.ceil(containerHeight / 20),
        });
      }
    }
  }, [active, files, focusedIndex]);

  useEffect(() => {
    if (active) {
      if (filter === null) {
        containerRef.current?.focus();
      } else {
        inputRef.current?.focus();
      }
    } else {
      inputRef.current?.blur();
      containerRef.current?.blur();
    }
  }, [active, path, filter]);

  const open = async (file: File) => {
    if (!file) return;

    if (file.is_dir) {
      command("navigate", { path: file.name });
    } else {
      command("open", { filename: file.name });
    }
  };

  const relativeJump = (delta: number, withSelection?: boolean) => {
    command("relative_jump", { offset: delta, withSelection: !!withSelection });
  };

  const onKeyDownCommon = (e) => {
    if (e.key == "ArrowDown") {
      relativeJump(1, e.shiftKey);
    } else if (e.key == "ArrowUp") {
      relativeJump(-1, e.shiftKey);
    } else if (e.key == "PageDown") {
      relativeJump(10, e.shiftKey);
    } else if (e.key == "PageUp") {
      relativeJump(-10, e.shiftKey);
    } else if (e.key == "Home") {
      relativeJump(-Math.pow(2, 31), e.shiftKey);
    } else if (e.key == "End") {
      relativeJump(Math.pow(2, 31) - 1, e.shiftKey);
    } else if (e.key == "Enter") {
      if (e.ctrlKey) {
        command("send_to_terminal", { filename: files[focusedIndex].name });
      } else {
        open(files[focusedIndex]);
      }
    } else if (e.key == "Tab") {
      invoke("focus", { paneHandle: 1 - paneHandle });
    } else if (e.key == "." && e.ctrlKey) {
      command("copy_pane");
    } else if (e.key == "Escape") {
      command("cancel", {}, true);
      command("set_filter", { filter: null });
    } else if (e.key.toLowerCase() == "d" && e.ctrlKey) {
      command("deselect_all");
    } else if (e.key.toLowerCase() == "a" && e.ctrlKey) {
      command("select_all");
    } else if (e.key == "F3") {
      command("view", { filename: files[focusedIndex].name });
    } else if (e.key == "F2") {
      command("dialog", { dialog: "rename" });
    }
    if (e.key == "F7") {
      command("dialog", { dialog: "create_directory" });
    } else if (e.key.toLowerCase() == "l" && e.ctrlKey) {
      command("dialog", { dialog: "navigate" });
    } else if ((e.key.toLowerCase() == "c" || e.key == "Insert") && e.ctrlKey) {
      command("copy_to_clipboard");
    } else if (e.key == "Delete") {
      let message;
      if (selected.length > 0) {
        message = `Delete ${selected.length} selected files?`;
      } else {
        message = `Delete ${files[focusedIndex].name}?`;
      }

      confirm(message, { title: "Delete" }).then((confirmed) => {
        if (confirmed) {
          command("delete_selected");
        }
      });
    } else if (
      (e.key.toLowerCase() == "v" && e.ctrlKey) ||
      (e.key == "Insert" && e.shiftKey)
    ) {
      command("paste_from_clipboard", {}, true);
    } else if (e.key == "Insert") {
      command("toggle_selected", {
        filename: files[focusedIndex].name,
        focusNext: true,
      });
    } else {
      return false;
    }

    return true;
  };

  const onkeydown = (e) => {
    if (onKeyDownCommon(e)) {
      // ...
    } else if (e.key == "Backspace") {
      command("navigate", { path: ".." }, true);
    } else if (e.key.length == 1 && !e.ctrlKey && !e.shiftKey) {
      // Is this a good way to check for printable characters? Works for en-US,
      // but I have no idea how well it works for international IMEs.
      inputRef.current.focus();
      return;
    }

    e.preventDefault();
  };

  const onClick: React.MouseEventHandler<HTMLLIElement> = (e) => {
    e.stopPropagation();

    if (e.ctrlKey) {
      command("toggle_selected", {
        filename: e.currentTarget.dataset.name,
        focusNext: false,
      });
    } else if (e.shiftKey) {
      command("select_range", { filename: e.currentTarget.dataset.name });
    } else {
      command("focus", { filename: e.currentTarget.dataset.name });
    }
  };

  const onkeydownFilter: React.KeyboardEventHandler = (e) => {
    if (onKeyDownCommon(e)) {
      // ...
    } else if (e.key == "ArrowLeft") {
      if (filter.length > 0) {
        command("set_filter", {
          filter: focused.substring(0, filter.length - 1),
        });
      }
    } else if (e.key == "ArrowRight") {
      if (filter.length < focused.length) {
        command("set_filter", {
          filter: focused.substring(0, filter.length + 1),
        });
      }
    } else {
      return;
    }

    e.preventDefault();
  };

  const onScroll: React.UIEventHandler<HTMLElement> = (e) => {
    tableHeaderRef.current.scrollLeft = e.currentTarget.scrollLeft;
  };

  const widthPrefix = `pane-${paneHandle}-column-`;

  return (
    <div
      className={`pane ${showSpinner ? "pane-busy" : ""}`}
      onClick={() => command("focus")}
    >
      <input
        className="filter-input"
        type="text"
        value={filter || ""}
        onChange={(e) => command("set_filter", { filter: e.target.value })}
        ref={inputRef}
        onKeyDown={onkeydownFilter}
        onFocus={() => command("set_filter", { filter: filter || "" })}
        tabIndex={-1}
      />
      <div className="header">{pending_path || path}</div>
      <div className="table-header" ref={tableHeaderRef}>
        <div className="table-header-inner">
          {columns.map((column) => (
            <ColumnHeader
              key={column.key}
              widthPrefix={widthPrefix}
              sorting={sorting}
              column={column}
              onSort={(key, asc) => {
                command("set_sorting", {
                  sorting: { key, asc },
                });
              }}
            />
          ))}
        </div>
      </div>
      {files && (
        <ul
          className="files"
          ref={containerRef}
          onKeyDown={onkeydown}
          tabIndex={-1}
          onScroll={onScroll}
        >
          <ViewportList
            overscan={0}
            initialIndex={focusedIndex}
            ref={viewPortRef}
            viewportRef={containerRef}
            items={files}
            itemSize={20}
          >
            {(row: File) => (
              <li
                key={row.name}
                data-name={row.name}
                className={`file-item ${
                  active && row.name == focused ? "focused" : ""
                } ${selectedLookup.has(row.name) ? "selected" : ""}`}
                onClick={onClick}
                onDoubleClick={() => open(row)}
              >
                {columns.map((column) => (
                  <div
                    key={column.key}
                    style={{
                      textAlign: column.align,
                      width: `var(--${widthPrefix}-${column.key})`,
                    }}
                    className="datum"
                  >
                    {column.render(row, props)}
                  </div>
                ))}
              </li>
            )}
          </ViewportList>
        </ul>
      )}
      <div className="statusbar">
        {showSpinner && "Loading file list..."}
        {!showSpinner && selected.length > 0 && (
          <>
            {selectedFileCount} files, {selectedDirCount} directories selected,{" "}
            {selectedBytes.toLocaleString()} bytes total
          </>
        )}
        {!showSpinner && selected.length == 0 && (
          <>
            {fileCount} files, {dirCount} directories
          </>
        )}
      </div>
    </div>
  );
}

function Terminal({ handle, active }: { handle: string; active: boolean }) {
  const terminalRef = useRef<XTermJSTerminal>(null);
  const ref = useRef<HTMLDivElement>(null);
  const termDataContext = useContext(TerminalData);

  useEffect(() => {
    const term = new XTermJSTerminal({
      scrollback: 1000,
      fontFamily:
        "Consolas, Menlo, Monaco, 'Lucida Console', 'Liberation Mono', 'DejaVu Sans Mono', 'Bitstream Vera Sans Mono', 'Courier New', monospace, serif",
      fontSize: 12,
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

    term.element.addEventListener("focus", () => {
      safeCommandSilent("terminal_focus", { handle });
    });

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
  }, [active]);

  return (
    <div className="terminal-container">
      <div className="terminal" ref={ref} />
    </div>
  );
}

function App() {
  const remoteState = useRemoteState<MainWindowState>("main_window", []);
  const terminalData = useTerminalData([]);

  const onkeydown = (e) => {
    if (e.key.toLowerCase() == "h" && e.ctrlKey) {
      safeCommand("toggle_hidden");
    } else if (e.key.toLowerCase() == "n" && e.ctrlKey) {
      safeCommand("new_window");
    } else if (e.key.toLowerCase() == "w" && e.ctrlKey) {
      window.close();
    } else {
      return;
    }
    e.preventDefault();
  };

  useEffect(() => {
    window.addEventListener("keydown", onkeydown);
    return () => window.removeEventListener("keydown", onkeydown);
  }, []);

  return (
    <Profiler id="app" onRender={console.log}>
      <TerminalData.Provider value={terminalData}>
        <ReactModal
          isOpen={!!remoteState?.modal}
          onRequestClose={() => safeCommand("close_modal")}
          overlayClassName={"modal-overlay"}
          className={"modal-content"}
        >
          <ModalContent state={remoteState?.modal} />
        </ReactModal>
        <Allotment vertical className="container" separator>
          <Allotment minSize={200}>
            {remoteState &&
              remoteState.panes.map((props, i) => (
                <Pane
                  key={i}
                  paneHandle={i}
                  {...props}
                  active={
                    remoteState.display_options.panes_focused &&
                    remoteState.display_options.active_pane === i
                  }
                />
              ))}
          </Allotment>
          {remoteState &&
            Object.keys(remoteState.terminals).map((handle) => (
              <Allotment.Pane preferredSize="20%" key={handle}>
                <Terminal
                  handle={handle}
                  active={
                    !remoteState.display_options.panes_focused &&
                    remoteState.display_options.active_terminal === handle
                  }
                />
              </Allotment.Pane>
            ))}
        </Allotment>
      </TerminalData.Provider>
    </Profiler>
  );
}

export default App;
