import { useState, useEffect, useRef, useMemo, useCallback } from "react";
import { invoke } from "@tauri-apps/api/tauri";
import { message } from "@tauri-apps/api/dialog";

import { Allotment } from "allotment";
import "allotment/dist/style.css";

import iconMapping from "./assets/mapping.json";
import {
  HotkeysProvider,
  useHotkeys,
  useHotkeysContext,
} from "react-hotkeys-hook";
import { ViewportList, ViewportListRef } from "react-viewport-list";
import { CSSProperties } from "react";
import { Profiler } from "react";

type File = {
  id: string;
  name: string;
  size: number;
  is_dir: boolean;
  is_hidden: boolean;
  mode: number;
  modified: string;
  accessed: string;
  created: string;
};

type NavigateParams = {
  path?: string;
  up?: boolean;
  otherPane?: boolean;
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

function Pane({ path, active, onFocus, navigate, initialFile }) {
  const [files, setFiles] = useState<File[]>(null);
  const [focusedFile, setFocusedFile] = useState<string>(initialFile);
  const [sorting, setSorting] = useState({ key: "name", asc: true });
  const [filter, setFilter] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const focusedIndex = useMemo(() => {
    if (files) {
      return Math.max(0, files.findIndex((f) => f.name === focusedFile));
    }
    return 0;
  }, [files, focusedFile]);

  const containerRef = useRef<HTMLUListElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const viewPortRef = useRef<ViewportListRef>(null);

  const scrollTo = (index) => {
    // Do this first rather than after the state update to avoid flickering
    // when someone holds down the arrow keys.
    if (active && files && viewPortRef.current) {
      const containerHeight = containerRef.current!.offsetHeight;
      const pos = viewPortRef.current.getScrollPosition();
      if (index < pos.index || (index == pos.index && pos.offset > 0)) {
        viewPortRef.current.scrollToIndex({
          index: index,
          delay: 10,
          alignToTop: true,
        });
      } else if (index >= pos.index + Math.floor(containerHeight / 20)) {
        viewPortRef.current.scrollToIndex({
          index: index,
          delay: 10,
          alignToTop: false,
        });
      }
      setFocusedFile(files[index].name);
    }
  };

  const getFiles = async (preserveScroll: boolean) => {
    const newFiles: File[] = await invoke("directory_list", { path, sorting });

    setFilter(null);
    setFiles(newFiles);
    setFocusedFile(initialFile);
  };

  useEffect(() => {
    getFiles(false);
    setSelected(new Set());
  }, [path]);

  useEffect(() => {
    getFiles(true);
  }, [sorting]);

  const open = async (file: File) => {
    if (!file) return;

    if (file.is_dir) {
      navigate({ path: file.name });
    } else {
      await invoke("open", { basePath: path, path: file.name });
    }
  };

  const relativeJump = (delta: number, nofilter?: boolean) => {
    if (!files || delta === 0) return;

    let newIndex;
    if (nofilter || filter === null) {
      newIndex = Math.max(0, Math.min(focusedIndex + delta, files.length - 1));
    } else {
      let remaining = delta;
      let i = focusedIndex;
      newIndex = focusedIndex;
      do {
        i += Math.sign(delta);
        if (i < 0 || i >= files.length || remaining === 0) {
          break;
        }
        if (files[i].name.toLocaleLowerCase().startsWith(filter)) {
          newIndex = i;
          remaining -= Math.sign(delta);
        }
      } while (true);
    }
    scrollTo(newIndex);
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
      relativeJump(-Infinity);
    } else if (e.key == "End") {
      relativeJump(Infinity);
    } else if (e.key == "Enter") {
      open(files[focusedIndex]);
    } else if (e.key == "Tab") {
      onFocus(false);
    } else if (e.key == "." && e.ctrlKey) {
      navigate({ otherPane: true });
    } else if (e.key == "Escape") {
      containerRef.current.focus();
      setFilter(null);
    } else if (e.key == "Insert") {
      setFilter(null);
      setSelected((selected) => {
        const newSelected = new Set(selected);

        if (selected.has(focusedFile)) {
          newSelected.delete(focusedFile);
        } else {
          newSelected.add(focusedFile);
        }

        return newSelected;
      });
      relativeJump(1, true);
    } else if (e.key.toLowerCase() == "d" && e.ctrlKey) {
      setFilter(null);
      setSelected(new Set());
    } else if (e.key.toLowerCase() == "a" && e.ctrlKey) {
      setFilter(null);
      setSelected(new Set(files.map((f) => f.name)));
    } else {
      return false;
    }

    return true;
  };

  const onkeydown = (e) => {
    console.log("onkeydown", e.key);
    if (onKeyDownCommon(e)) {
      // ...
    } else if (e.key == "Backspace") {
      navigate({ up: true });
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
        setFilter(focusedFile.substring(0, filter.length - 1));
      }
    } else if (e.key == "ArrowRight") {
      if (filter.length < focusedFile.length) {
        setFilter(focusedFile.substring(0, filter.length + 1));
      }
    } else {
      return;
    }

    e.preventDefault();
  };

  const applyFilter = (newFilter: string) => {
    console.log("applyFilter", newFilter);

    let newIndex = null;
    // Search in the forward direction first. If the current file matches, then
    // we'll stay on it.
    for (let i = focusedIndex; i < files.length; i++) {
      if (files[i].name.toLocaleLowerCase().startsWith(newFilter)) {
        newIndex = i;
        break;
      }
    }
    if (newIndex === null) {
      for (let i = focusedIndex; i >= 0; i--) {
        if (files[i].name.toLocaleLowerCase().startsWith(newFilter)) {
          newIndex = i;
          break;
        }
      }
    }
    if (newIndex !== null) {
      setFilter(newFilter);
      scrollTo(newIndex);
    } else if (filter === null) {
      // Enter the filter mode even if the first character doesn't match.
      setFilter("");
    }
  };

  useEffect(() => {
    if (active && files && inputRef.current) {
      containerRef.current.focus();
      scrollTo(focusedIndex);
    }
    setFilter(null);
  }, [active, files]);

  return (
    <div className="pane" onClick={() => onFocus(true)}>
      <input
        className="filter-input"
        type="text"
        value={filter || ""}
        onChange={(e) => applyFilter(e.target.value.toLocaleLowerCase())}
        ref={inputRef}
        onKeyDown={onkeydownFilter}
        tabIndex={-1}
      />
      <div className="header">{path}</div>
      <div className="table-header">
        {columns.map(({ name, key, sortable, style }, i) => (
          <div
            className="column"
            style={style}
            key={i}
            onClick={() => sortable && setSorting({ key, asc: !sorting.asc })}
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
            {(row, i) => (
              <li
                key={i}
                className={
                  `file-item ${(active && focusedIndex === i) ? "focused" : ""} ${selected.has(row.name) ? "selected" : ""}`
                }
                onClick={() => {
                  setFilter(null);
                  scrollTo(i);
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

type PaneState = {
  path: string;
  initialFile?: string;
};

function App() {
  const [activePane, setActivePane] = useState(0);
  const [paneStates, setPaneStates] = useState<PaneState[]>([
    { path: "/home/tibordp/src/alumina/src/alumina-boot" },
    { path: "/home/tibordp/" },
  ]);

  const focusNext = () => setActivePane((i) => (i + 1) % paneStates.length);
  const navigate = async (index: number, params: NavigateParams) => {
    try {
      const state = paneStates[index];

      let newpath;
      if (params.up) {
        newpath = "..";
      } else if (params.path) {
        newpath = params.path;
      } else if (params.otherPane) {
        newpath = paneStates[(index + 1) % paneStates.length].path;
      }

      const target: string = await invoke("navigate", {
        basePath: state.path,
        path: newpath,
      });
      const lastSegment = state.path.split("/").pop();

      setPaneStates((states) =>
        states.map((state, j) =>
          j === index
            ? {
                ...state,
                path: target,
                initialFile: newpath === ".." && lastSegment,
              }
            : state
        )
      );
    } catch (e) {
      await message(e.toString(), {
        type: "error",
        title: "Error",
      });
    }
  };

  return (
    <HotkeysProvider>
      <Profiler id="app" onRender={console.log}>
        <Allotment minSize={200} className="container">
          {paneStates.map((state, i) => (
            <Pane
              key={i}
              path={state.path}
              active={activePane === i}
              onFocus={(focused) => {
                if (focused) {
                  setActivePane(i);
                } else {
                  focusNext();
                }
              }}
              initialFile={state.initialFile}
              navigate={(params) => navigate(i, params)}
            />
          ))}
        </Allotment>
      </Profiler>
    </HotkeysProvider>
  );
}

export default App;
