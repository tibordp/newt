import { useState, useEffect, useRef, useMemo } from "react";
import { invoke } from "@tauri-apps/api/tauri";
import { listen, Event } from "@tauri-apps/api/event";
import { appWindow } from "@tauri-apps/api/window";
import { message } from "@tauri-apps/api/dialog";

import { Allotment } from "allotment";
import "./MainWindow.css";
import "allotment/dist/style.css";

import iconMapping from "../assets/mapping.json";
import { ViewportList, ViewportListRef } from "react-viewport-list";
import { CSSProperties } from "react";
import { Profiler } from "react";

type File = {
  name: string;
  size: number;
  is_dir: boolean;
  is_hidden: boolean;
  mode: number;
  modified: string;
  accessed: string;
  created: string;
};

function FileName({ focused, filter, info }) {
  const { name, is_dir, is_hidden } = info;

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

  return (
    <>
      {!is_dir && (
        <div className="filename">
          <div className="file-icon" style={{ color: fontColor }}>
            {ch}
          </div>
          <div className={focused ? "filename-part focused" : "filename-part"}>
            {nameElement}
          </div>
        </div>
      )}
      {is_dir && (
        <div className="filename">
          <div className="file-icon folder" />
          <div className={focused ? "filename-part focused" : "filename-part"}>
            {nameElement}
          </div>
        </div>
      )}
    </>
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
  sortable: boolean;
  style: CSSProperties;
};

const columns: ColumnDef[] = [
  {
    name: "Name",
    key: "name",
    sortable: true,
    style: {
      flexGrow: 4,
      flexShrink: 0,
      flexBasis: "100px",
      textAlign: "left",
    },
  },
  {
    name: "Size",
    key: "size",
    sortable: true,
    style: {
      flexGrow: 1,
      flexShrink: 0,
      flexBasis: "50px",
      textAlign: "right",
    },
  },
  {
    name: "Date",
    key: "modified",
    sortable: true,
    style: {
      flexGrow: 1,
      flexShrink: 0,
      flexBasis: "30px",
      textAlign: "center",
    },
  },
  {
    name: "Time",
    key: "modified",
    sortable: true,
    style: {
      flexGrow: 1,
      flexShrink: 0,
      flexBasis: "30px",
      textAlign: "center",
    },
  },
  {
    name: "Mode",
    key: "mode",
    sortable: false,
    style: {
      flexGrow: 1,
      flexShrink: 0,
      flexBasis: "30px",
      textAlign: "center",
    },
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

type GlobalState = {
  panes: PaneState[];
};

type ChangePayload = {
  state: GlobalState;
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
        setState((s) => deepUpdate(s, event.payload.state));
      }
    });
    listenPromise.then(() => invoke("ping", {}));
    return () => {
      listenPromise.then((unlisten) => unlisten());
    };
  }, deps);

  return state;
};

function Pane({
  paneHandle,
  active,
  filter,
  path,
  files,
  selected,
  sorting,
  focused,
}: PaneState & { paneHandle: number }) {
  const command = async (cmd: string, args: any = {}) => {
    try {
      await invoke(cmd, { paneHandle, ...args });
    } catch (e) {
      await message(e.toString(), {
        type: "error",
        title: "Error",
      });
    }
  };

  // Without this lookup, rendering suddenly becomes O(n^2), which is very slow
  // when someone Ctrl+A's a directory with 1000+ files.
  const selectedLookup = useMemo(() => {
    return new Set(selected);
  }, [selected]);

  const focusedIndex = useMemo(() => {
    if (files) {
      return files.findIndex((f) => f.name === focused);
    }
    return -1;
  }, [files, focused]);

  const containerRef = useRef<HTMLUListElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const viewPortRef = useRef<ViewportListRef>(null);

  useEffect(() => {
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
        });
      } else if (focusedIndex >= pos.index + Math.floor(containerHeight / 20)) {
        viewPortRef.current.scrollToIndex({
          index: focusedIndex,
          delay: 0,
          alignToTop: false,
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

  const relativeJump = (delta: number, nofilter?: boolean) => {
    command("relative_jump", { offset: delta });
  };

  const onKeyDownCommon = (e) => {
    if (e.key == "ArrowDown") {
      relativeJump(1);
    } else if (e.key == "ArrowUp") {
      relativeJump(-1);
    } else if (e.key == "PageDown") {
      relativeJump(10);
    } else if (e.key == "PageUp") {
      relativeJump(-10);
    } else if (e.key == "Home") {
      relativeJump(-Math.pow(2, 31));
    } else if (e.key == "End") {
      relativeJump(Math.pow(2, 31) - 1);
    } else if (e.key == "Enter") {
      open(files[focusedIndex]);
    } else if (e.key == "Tab") {
      invoke("focus", { paneHandle: 1 - paneHandle });
    } else if (e.key == "." && e.ctrlKey) {
      command("copy_pane");
    } else if (e.key == "Escape") {
      command("set_filter", { filter: null });
    } else if (e.key == "Insert") {
      command("toggle_selected");
    } else if (e.key.toLowerCase() == "d" && e.ctrlKey) {
      command("deselect_all");
    } else if (e.key.toLowerCase() == "a" && e.ctrlKey) {
      command("select_all");
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
    } else if (e.key.length == 1) {
      // Is this a good way to check for printable characters? Works for en-US,
      // but I have no idea how well it works for international IMEs.
      inputRef.current.focus();
      return;
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
      <div className="header">
        {path}
      </div>
      <div className="table-header">
        {columns.map(({ name, key, sortable, style }, i) => (
          <div
            className="column"
            style={style}
            key={i}
            onClick={() =>
              sortable &&
              command("set_sorting", { sorting: { key, asc: !sorting.asc } })
            }
          >
            {name}
            {sorting.key == key && (
              <span className="sort-indicator">
                {sorting.asc ? " ▲" : " ▼"}
              </span>
            )}
          </div>
        ))}
      </div>
      {files && (
        <ul
          className="files"
          ref={containerRef}
          onKeyDown={onkeydown}
          tabIndex={0}
        >
          <ViewportList
            overscan={10}
            initialIndex={focusedIndex}
            ref={viewPortRef}
            viewportRef={containerRef}
            items={files}
          >
            {(row: File, i) => (
              <li
                key={row.name}
                className={`file-item ${
                  active && focusedIndex === i ? "focused" : ""
                } ${selectedLookup.has(row.name) ? "selected" : ""}`}
                onClick={() => {
                  command("focus", { filename: row.name });
                }}
                onDoubleClick={() => open(row)}
              >
                <div style={columns[0].style} className="datum">
                  <FileName
                    filter={filter}
                    focused={active && focusedIndex == i}
                    info={row}
                  />
                </div>
                <div style={columns[1].style} className="align-right datum">
                  {row.is_dir ? "DIR" : row.size.toLocaleString()}
                </div>
                <div style={columns[3].style} className="align-center datum">
                  {new Date(row.modified).toLocaleDateString()}
                </div>
                <div style={columns[4].style} className="align-center datum">
                  {new Date(row.modified).toLocaleTimeString()}
                </div>
                <div style={columns[2].style} className="align-center datum">
                  {modeToString(row.mode)}
                </div>
              </li>
            )}
          </ViewportList>
        </ul>
      )}
    </div>
  );
}

function App() {
  const remoteState = useRemoteState([]);

  const onkeydown = (e) => {
    if (e.key.toLowerCase() == "n" && e.ctrlKey) {
      invoke("new_window", {});
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
      <div style={{ position: "fixed", top: 0, zIndex: 10000, backgroundColor: "red", padding: "1em", margin: "1em" }}>{window.location.href}</div>
      <Allotment minSize={200} className="container">
        {remoteState &&
          remoteState.panes.map((props, i) => (
            <Pane key={i} paneHandle={i} {...props} />
          ))}
      </Allotment>
    </Profiler>
  );
}

export default App;
