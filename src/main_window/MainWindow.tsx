import {
  useState,
  useEffect,
  useRef,
  useMemo,
  useLayoutEffect,
  startTransition,
  useId,
} from "react";
import { invoke } from "@tauri-apps/api/tauri";
import { listen, Event } from "@tauri-apps/api/event";
import { appWindow } from "@tauri-apps/api/window";

import { Allotment } from "allotment";
import "./MainWindow.css";
import "allotment/dist/style.css";

import iconMapping from "../assets/mapping.json";
import { ViewportList, ViewportListRef } from "../lib/viewPortList";
import { CSSProperties } from "react";
import { Profiler } from "react";
import { enablePatches, applyPatches, Patch } from "immer";

import { safeCommand } from "../lib/invoke";
import { Terminal } from "xterm";
import { CanvasAddon } from "xterm-addon-canvas";
import { FitAddon } from "xterm-addon-fit";

import "xterm/css/xterm.css";
import { v4 as uuidv4 } from 'uuid';

import "@fontsource-variable/roboto-mono";

enablePatches();

type File = {
  name: string;
  size: number;
  is_dir: boolean;
  is_symlink: boolean;
  is_hidden: boolean;
  mode: number;
  modified: string;
  accessed: string;
  created: string;
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

function deepUpdate(original: any, received: any): any {
  if (
    original === null ||
    received === null ||
    Array.isArray(original) !== Array.isArray(received) ||
    typeof original !== typeof received
  ) {
    return received;
  }

  let isChanged = false;
  let ret;
  if (Array.isArray(original)) {
    if (original.length !== received.length) {
      return received;
    }

    const result = Array(original.length);
    for (let i = 0; i < original.length; i++) {
      result[i] = deepUpdate(original[i], received[i]);
      isChanged = isChanged || result[i] !== original[i];
    }

    ret = isChanged ? result : original;
  } else if (typeof original === "object") {
    const keys = new Set([...Object.keys(original), ...Object.keys(received)]);

    const result = {};
    for (const key of keys) {
      result[key] = deepUpdate(original[key], received[key]);
      isChanged = isChanged || result[key] !== original[key];
    }
    ret = isChanged ? result : original;
  } else {
    ret = received;
  }

  return ret;
}

function modeToString(mode) {
  const types = ["-", "d", "l"]; // File types: Regular file, Directory, Symbolic link
  const permissions = ["---", "--x", "-w-", "-wx", "r--", "r-x", "rw-", "rwx"]; // Permission strings

  const actualMode = mode & 0o7777;

  const type = types[Math.floor(actualMode / (8 * 8 * 8))]; // Get the file type
  const owner = permissions[Math.floor((actualMode % (8 * 8 * 8)) / (8 * 8))]; // Get the owner's permissions
  const group = permissions[Math.floor((actualMode % (8 * 8)) / 8)]; // Get the group's permissions
  const other = permissions[actualMode % 8]; // Get the permissions for others

  return type + owner + group + other + ".";
}

type ColumnDef = {
  name: string;
  key: string;
  sortKey?: string;
  align: "left" | "right" | "center";
  render: (info: File, paneProps: PaneState) => JSX.Element;
  initialWidth: number;
};

const columns: ColumnDef[] = [
  {
    name: "Name",
    key: "name",
    sortKey: "name",
    align: "left",
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
    name: "Size",
    key: "size",
    sortKey: "size",
    align: "right",
    render: (info) => <>{info.is_dir ? "DIR" : info.size.toLocaleString()}</>,
    initialWidth: 100,
  },
  {
    name: "Date",
    key: "modified_date",
    sortKey: "modified",
    align: "right",
    render: (info) => <>{new Date(info.modified).toLocaleDateString()}</>,
    initialWidth: 70,
  },
  {
    name: "Time",
    key: "modified_time",
    sortKey: "modified",
    align: "right",
    render: (info) => <>{new Date(info.modified).toLocaleTimeString()}</>,
    initialWidth: 70,
  },
  {
    name: "Mode",
    key: "mode",
    align: "right",
    render: (info) => <>{modeToString(info.mode)}</>,
    initialWidth: 70,
  },
];

type Sorting = {
  key: string;
  asc: boolean;
};

type PaneState = {
  path: string;
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
};

type GlobalState = {
  panes: PaneState[];
  display_options: DisplayOptions;
};

type ChangePayload = {
  state?: GlobalState;
  patch?: Patch[];
};

const useRemoteState = (deps: any[] = []): GlobalState | null => {
  const [state, setState] = useState<any>(null);

  useEffect(() => {
    let listenPromise = listen("updated", (event: Event<ChangePayload>) => {
      if (event.windowLabel === appWindow.label) {
        // State is serialized, so we perform a "deep" update (diff), updating
        // only the changed parts of the current state. This is to avoid losing
        // the reference to the state object, which would cause a re-render of
        // the entire component tree.
        setState((s) => {
          const start = performance.now();
          let ret;
          if (event.payload.patch) {
            console.log(event.payload.patch);
            ret = applyPatches(s, event.payload.patch);
          } else {
            ret = deepUpdate(s, event.payload.state!);
          }
          const end = performance.now();
          console.log(`[useRemoteState] Update took ${end - start}ms`);
          return ret;
        });
      }
    });
    listenPromise.then(() => invoke("ping", {}));
    return () => {
      listenPromise.then((unlisten) => unlisten());
    };
  }, deps);

  return state;
};

function ColumnHeader({ widthPrefix, column, sorting, onClick }) {
  const { name, key, sortKey } = column;
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

  return (
    <>
      <div
        ref={ref}
        className={`column ${sortKey ? "sortable" : ""} ${
          sorting.key == key && sorting.asc ? "sorted-asc" : ""
        } ${sorting.key == key && !sorting.asc ? "sorted-desc" : ""}`}
        style={{
          width: `var(--${widthPrefix}-${column.key})`,
        }}
        onClick={onClick}
      >
        {name}
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
  } = props;
  const command = (cmd: string, args: object = {}) =>
    safeCommand(cmd, { paneHandle, ...args });

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
    if (active && containerRef.current && inputRef.current) {
      if (filter === null) {
        containerRef.current.focus();
      } else {
        inputRef.current.focus();
      }
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
      open(files[focusedIndex]);
    } else if (e.key == "Tab") {
      invoke("focus", { paneHandle: 1 - paneHandle });
    } else if (e.key == "." && e.ctrlKey) {
      command("copy_pane");
    } else if (e.key == "Escape") {
      command("set_filter", { filter: null });
    } else if (e.key.toLowerCase() == "d" && e.ctrlKey) {
      command("deselect_all");
    } else if (e.key.toLowerCase() == "a" && e.ctrlKey) {
      command("select_all");
    } else if (e.key == "F3") {
      command("view", { filename: files[focusedIndex].name });
    } else if ((e.key.toLowerCase() == "c" || e.key == "Insert") && e.ctrlKey) {
      command("copy_to_clipboard");
    } else if (
      (e.key.toLowerCase() == "v" && e.ctrlKey) ||
      (e.key == "Insert" && e.shiftKey)
    ) {
      command("paste_from_clipboard");
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
      invoke("navigate", { paneHandle, path: ".." });
    } else if (e.key.length == 1 && !e.ctrlKey && !e.shiftKey) {
      // Is this a good way to check for printable characters? Works for en-US,
      // but I have no idea how well it works for international IMEs.
      inputRef.current.focus();
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
    e.preventDefault();
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
    <div className="pane" onClick={() => command("focus")}>
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
      <div className="header">{path}</div>
      <div className="table-header" ref={tableHeaderRef}>
        <div className="table-header-inner">
          {columns.map((column) => (
            <ColumnHeader
              key={column.key}
              widthPrefix={widthPrefix}
              sorting={sorting}
              column={column}
              onClick={() =>
                column.sortKey &&
                command("set_sorting", {
                  sorting: { key: column.sortKey, asc: !sorting.asc },
                })
              }
            />
          ))}
        </div>
      </div>
      {files && (
        <ul
          className="files"
          ref={containerRef}
          onKeyDown={onkeydown}
          tabIndex={0}
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
        {selected.length > 0 && (
          <>
            {selectedFileCount} files, {selectedDirCount} directories selected,{" "}
            {selectedBytes.toLocaleString()} bytes total
          </>
        )}
        {selected.length == 0 && (
          <>
            {fileCount} files, {dirCount} directories
          </>
        )}
      </div>
    </div>
  );
}

function XTerm() {
  const outer = useRef<HTMLDivElement>(null);
  const ref = useRef<HTMLDivElement>(null);
  const [addon, setAddon] = useState<Terminal | null>(null);
  const [term, setTerm] = useState<Terminal | null>(null);

  useEffect(() => {
    const t = new Terminal({
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
    t.open(ref.current!);
    const addon = new CanvasAddon();
    const fitAddon = new FitAddon();
    //t.loadAddon(addon);
    t.loadAddon(fitAddon);
    fitAddon.fit();
    const resizeObserver = new ResizeObserver(() => {
      console.log("resize");
      fitAddon.fit();
    });
    resizeObserver.observe(ref.current!);
    setTerm(t);

    const init = async () => {
      const handle = uuidv4();

      const listener = await listen("terminal_data", (data) => {
        if (data.payload.handle === handle) {
          t.write(data.payload.data);
        }
      });

      await invoke("terminal_open", { handle, rows: t.rows, cols: t.cols });
      const binaryData = new TextEncoder().encode("hello world");
      console.log(JSON.stringify(binaryData));

      invoke("terminal_write", { handle, data: [...binaryData] })
        .then(() => {
          console.log("written");
        })
        .catch((e) => {
          console.error(e);
        });
      t.onBinary((data) => {
        const binaryData = new TextEncoder().encode(data);
        console.log(JSON.stringify(binaryData));

        invoke("terminal_write", { handle, data: [...binaryData] })
          .then(() => {
            console.log("written");
          })
          .catch((e) => {
            console.error(e);
          });
      });
      t.onResize((size) => {
        invoke("terminal_resize", { handle, rows: size.rows, cols: size.cols })
          .then(() => {
            console.log("resized");
          })
          .catch((e) => {
            console.error(e);
          });
      });
      t.onData((data) => {
        const binaryData = new TextEncoder().encode(data);
        console.log(JSON.stringify(binaryData));
        invoke("terminal_write", { handle, data: [...binaryData] })
          .then(() => {
            console.log("written");
          })
          .catch((e) => {
            console.error(e);
          });
      });
    };
    init()
      .then(() => {
        console.log("inited");
      })
      .catch((e) => {
        console.error(e);
      });

    return () => {
      t.dispose();
      resizeObserver.unobserve(ref.current!);
    };
  }, []);

  return (
    <div className="terminal-container">
      <div className="terminal" ref={ref} />
    </div>
  );
}

function App() {
  const remoteState = useRemoteState([]);
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
      <Allotment vertical className="container" separator>
        <Allotment minSize={200}>
          {remoteState &&
            remoteState.panes.map((props, i) => (
              <Pane
                key={i}
                paneHandle={i}
                {...props}
                active={remoteState.display_options.active_pane === i}
              />
            ))}
        </Allotment>
        <XTerm />
      </Allotment>
    </Profiler>
  );
}

export default App;
