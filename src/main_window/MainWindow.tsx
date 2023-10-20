import {
  useState,
  useEffect,
  useRef,
  useMemo,
  useLayoutEffect,
  useContext,
  Fragment,
  useCallback,
} from "react";
import { invoke } from "@tauri-apps/api/tauri";

import { Allotment } from "allotment";
import "./MainWindow.scss";
import "allotment/dist/style.css";


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
import { commands, executeCommand, modifiers } from "../lib/commands";
import CommandPalette from "./modals/CommandPalette";

enablePatches();

const SI_PREFIXES_CENTER_INDEX = 10;

const siPrefixes: readonly string[] = [
  "q",
  "r",
  "y",
  "z",
  "a",
  "f",
  "p",
  "n",
  "μ",
  "m",
  "",
  "k",
  "M",
  "G",
  "T",
  "P",
  "E",
  "Z",
  "Y",
  "R",
  "Q",
];

export const getSiPrefixedNumber = (number: number): string => {
  if (number === 0) return number.toString();
  const EXP_STEP_SIZE = 3;
  const base = Math.floor(Math.log10(Math.abs(number)));
  const siBase = (base < 0 ? Math.ceil : Math.floor)(base / EXP_STEP_SIZE);
  const prefix = siPrefixes[siBase + SI_PREFIXES_CENTER_INDEX];

  // return number as-is if no prefix is available
  if (siBase === 0) return number.toString();

  // We're left with a number which needs to be devided by the power of 10e[base]
  // This outcome is then rounded two decimals and parsed as float to make sure those
  // decimals only appear when they're actually requird (10.0 -> 10, 10.90 -> 19.9, 10.01 -> 10.01)
  const baseNumber = parseFloat(
    (number / Math.pow(10, siBase * EXP_STEP_SIZE)).toFixed(2)
  );
  return `${baseNumber} ${prefix}`;
};

/*
In rust:
pub fn mode_string(mode: u32) -> String {
    const TYPE_CHARS: &[u8] = b"?pc?d?b?-?l?s???";
    const MODE_CHARS: &[u8] = b"rwxSTst";

    let mut ret = vec![0; 10];
    let mut idx = 0usize;

    ret[idx] = TYPE_CHARS[((mode >> 12) & 0xf) as usize];
    let mut i = 0;
    let mut m = 0o400;
    loop {
        let mut j = 0;
        let mut k = 0;

        loop {
            idx += 1;
            ret[idx] = b'-';
            if mode & m != 0 {
                ret[idx] = MODE_CHARS[j];
                k = j;
            }
            m >>= 1;
            j += 1;
            if j >= 3 {
                break;
            }
        }
        i += 1;

        if mode & (0o10000 >> i) != 0 {
            ret[idx] = MODE_CHARS[3 + (k & 2) + ((i == 3) as usize)];
        }
        if i >= 3 {
            break;
        }
    }

    unsafe { String::from_utf8_unchecked(ret) }
}


*/


const modeString = (mode: number) => {
  const TYPE_CHARS = "?pc?d?b?-?l?s???";
  const MODE_CHARS = "rwxSTst";

  let ret = Array(10).fill("-");
  let idx = 0;

  ret[idx] = TYPE_CHARS[(mode >> 12) & 0xf];
  let i = 0;
  let m = 0o400;
  while (true) {
    let j = 0;
    let k = 0;

    while (true) {
      idx += 1;
      ret[idx] = "-";
      if ((mode & m) != 0) {
        ret[idx] = MODE_CHARS[j];
        k = j;
      }
      m = m >> 1;
      j += 1;
      if (j >= 3) {
        break;
      }
    }
    i += 1;

    if ((mode & (0o10000 >> i)) != 0) {
      ret[idx] = MODE_CHARS[3 + (k & 2) + ((i == 3) ? 1 : 0)];
    }
    if (i >= 3) {
      break;
    }
  }

  return ret.join("");
}


type File = {
  name: string;
  size?: number;
  is_dir: boolean;
  is_symlink: boolean;
  is_hidden: boolean;
  user: number | string;
  group: number | string;
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
      className={`filename ${is_hidden ? "hidden-file" : ""} ${is_symlink ? "symlink" : ""
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
    initialWidth: 80,
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
    initialWidth: 80,
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
    key: "user",
    subcolumns: [
      {
        name: "User",
        sortKey: "user",
      },
    ],
    render: (info) => <>{info.user}</>,
  },
  {
    align: "left",
    initialWidth: 70,
    key: "group",
    subcolumns: [
      {
        name: "Group",
        sortKey: "group",
      },
    ],
    render: (info) => <>{info.group}</>,
  },
  {
    align: "left",
    initialWidth: 70,
    key: "mode",
    subcolumns: [
      {
        name: "Mode",
        sortKey: "mode",
      },
    ],
    render: (info) => <>{modeString(info.mode)}</>,
  },
];

type Sorting = {
  key: string;
  asc: boolean;
};

type FsStats = {
  available_bytes: number;
  free_bytes: number;
  total_bytes: number;
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
  fs_stats?: FsStats;
};

type DisplayOptions = {
  show_hidden: boolean;
  active_pane: number;
  panes_focused: boolean;
  active_terminal?: number;
};

type Terminal = {
  handle: number;
};

export type MainWindowState = {
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
            key={i}
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

function PathBreadcrumbs(props: { path: string; paneHandle: number }) {
  let { path, paneHandle } = props;

  let segments = ["/", ...path.split("/").filter((s) => s)];
  let joined = segments.map((segment, i) => {
    if (i == 0) {
      return ["/", "/"];
    } else if (i === segments.length - 1) {
      return [segment, segments.slice(0, i + 1).join("/")];
    } else {
      return [`${segment}/`, segments.slice(0, i + 1).join("/")];
    }
  });

  return (
    <>
      {joined.map(([segment, path], i) => (
        <Fragment key={i}>
          <a
            className="path-breadcrumb"
            href="#"
            onClick={(e) => {
              e.preventDefault();
              if (i === segments.length - 1) {
                safeCommand("dialog", { paneHandle, dialog: "navigate" });
              } else {
                safeCommand("navigate", { paneHandle, path });
              }
            }}
          >
            {segment}
          </a>
        </Fragment>
      ))}
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
    fs_stats,
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

  const onKeyDownCommon = (e: React.KeyboardEvent<Element>) => {
    const { isMac, noModifiers, ctrlOrMeta, insertKey } = modifiers(e);

    if (e.key == "ArrowDown" && (noModifiers || e.shiftKey)) {
      relativeJump(1, e.shiftKey);
    } else if (e.key == "ArrowUp" && (noModifiers || e.shiftKey)) {
      relativeJump(-1, e.shiftKey);
    } else if (e.key == "PageDown" && (noModifiers || e.shiftKey)) {
      relativeJump(10, e.shiftKey);
    } else if (e.key == "PageUp" && (noModifiers || e.shiftKey)) {
      relativeJump(-10, e.shiftKey);
    } else if (e.key == "Home" && noModifiers) {
      relativeJump(-Math.pow(2, 31), e.shiftKey);
    } else if (e.key == "End" && noModifiers) {
      relativeJump(Math.pow(2, 31) - 1, e.shiftKey);
    } else if (e.key == "Enter" && noModifiers) {
      open(files[focusedIndex]);
    } else if (e.key == "Tab" && noModifiers) {
      invoke("focus", { paneHandle: 1 - paneHandle });
    } else if (e.key == "Escape" && noModifiers) {
      command("cancel", {}, true);
      command("set_filter", { filter: null });
    } else if (e.key == insertKey && noModifiers) {
      command("toggle_selected", {
        filename: files[focusedIndex].name,
        focusNext: true,
      });
    } else {
      return false;
    }

    return true;
  };

  const onkeydown = (e: React.KeyboardEvent<Element>) => {
    const { isMac, noModifiers, ctrlOrMeta, insertKey } = modifiers(e);

    if (onKeyDownCommon(e)) {
      // ...
    } else if (e.key == "Backspace" && noModifiers) {
      command("navigate", { path: ".." }, true);
    } else if (e.key.length == 1 && !e.ctrlKey && !e.shiftKey) {
      // Is this a good way to check for printable characters? Works for en-US,
      // but I have no idea how well it works for international IMEs.
      inputRef.current.focus();
      return;
    }

    e.preventDefault();
  };

  const onkeydownFilter: React.KeyboardEventHandler = (e) => {
    const { isMac, noModifiers, ctrlOrMeta, insertKey } = modifiers(e);

    if (onKeyDownCommon(e)) {
      // ...
    } else if (e.key == "ArrowLeft" && noModifiers) {
      if (filter.length > 0) {
        command("set_filter", {
          filter: focused.substring(0, filter.length - 1),
        });
      }
    } else if (e.key == "ArrowRight" && noModifiers) {
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

  const onClick: React.MouseEventHandler<HTMLLIElement> = (e) => {
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
      <div className="header">
        <div className="header-path">
          <PathBreadcrumbs
            path={pending_path || path}
            paneHandle={paneHandle}
          />
        </div>
        {fs_stats?.available_bytes !== undefined && (
          <div className="header-stats">
            {getSiPrefixedNumber(fs_stats.available_bytes)}B free
          </div>
        )}
      </div>
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
                className={`file-item ${active && row.name == focused ? "focused" : ""
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

function Terminal({ handle, active }: { handle: number; active: boolean }) {
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
    <div className="terminal-container" >
      <div className="terminal" ref={ref} tabIndex={-1} onFocus={() => safeCommandSilent("terminal_focus", { handle })} />
    </div>
  );
}

function App() {
  const remoteState = useRemoteState<MainWindowState>("main_window", []);
  const terminalData = useTerminalData([]);

  const [paletteOpen, setPaletteOpen] = useState(false);

  const onkeydown = useCallback((e) => {
    const { ctrlOrMeta } = modifiers(e);

    if (e.key.toLowerCase() == "p" && ctrlOrMeta) {
      setPaletteOpen(true);
    } else {
      for (const cmd of commands) {
        if (cmd.shortcut?.matches(e)) {
          if (executeCommand(cmd, remoteState) !== null) {
            e.preventDefault();
            return;
          }
        }
      }
      return;
    }

    e.preventDefault();
  }, [remoteState]);

  useEffect(() => {
    window.addEventListener("keydown", onkeydown);
    return () => window.removeEventListener("keydown", onkeydown);
  }, [onkeydown]);

  return (
    <Profiler id="app" onRender={console.log}>
      <TerminalData.Provider value={terminalData}>
        <ReactModal
          isOpen={!!remoteState?.modal}
          onRequestClose={() => safeCommand("close_modal")}
          overlayClassName={"modal-overlay"}
          className={"modal"}
          ariaHideApp={false}
        >
          <ModalContent state={remoteState?.modal} />
        </ReactModal>
        <ReactModal
          isOpen={paletteOpen}
          overlayClassName={"command-palette-overlay"}
          className={"command-palette"}
          ariaHideApp={false}
        >
          {remoteState && (
            <CommandPalette
              state={remoteState}
              onClose={() => setPaletteOpen(false)}
            />
          )}
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
            Object.values(remoteState.terminals).map((term) => (
              <Allotment.Pane preferredSize="20%" key={term.handle}>
                <Terminal
                  handle={term.handle}
                  active={
                    !remoteState.display_options.panes_focused &&
                    remoteState.display_options.active_terminal === term.handle
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
