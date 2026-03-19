import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { confirm, message } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import MonacoEditor, {
  type OnMount,
  type Monaco,
  loader,
} from "@monaco-editor/react";
import type { editor } from "monaco-editor";
import * as monaco from "monaco-editor";
import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import jsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import cssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import htmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import tsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";

self.MonacoEnvironment = {
  getWorker(_: unknown, label: string) {
    if (label === "json") return new jsonWorker();
    if (label === "css" || label === "scss" || label === "less")
      return new cssWorker();
    if (label === "html" || label === "handlebars" || label === "razor")
      return new htmlWorker();
    if (label === "typescript" || label === "javascript") return new tsWorker();
    return new editorWorker();
  },
};

loader.config({ monaco });

import styles from "./Editor.module.scss";
import { safeCommand, useRemoteState } from "../lib/ipc";
import type { VfsPath } from "../lib/types";

const MAX_FILE_SIZE = 5 * 1024 * 1024; // 5 MB

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

// Map file extensions to Monaco language IDs
const EXT_TO_LANGUAGE: Record<string, string> = {
  // Web
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "javascript",
  ts: "typescript",
  tsx: "typescript",
  html: "html",
  htm: "html",
  css: "css",
  scss: "scss",
  less: "less",
  json: "json",
  jsonc: "json",
  // Config
  yaml: "yaml",
  yml: "yaml",
  toml: "ini",
  ini: "ini",
  xml: "xml",
  svg: "xml",
  // Programming
  py: "python",
  rs: "rust",
  go: "go",
  java: "java",
  kt: "kotlin",
  kts: "kotlin",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  hxx: "cpp",
  cs: "csharp",
  rb: "ruby",
  php: "php",
  swift: "swift",
  m: "objective-c",
  r: "r",
  lua: "lua",
  pl: "perl",
  pm: "perl",
  // Shell
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  fish: "shell",
  ps1: "powershell",
  bat: "bat",
  cmd: "bat",
  // Data / Markup
  md: "markdown",
  mdx: "markdown",
  sql: "sql",
  graphql: "graphql",
  gql: "graphql",
  // DevOps
  dockerfile: "dockerfile",
  tf: "hcl",
  // Other
  diff: "diff",
  patch: "diff",
};

function detectLanguage(filePath: string, mimeType: string | null): string {
  // Try extension first
  const lastSegment = filePath.split("/").pop() ?? "";

  // Handle dotfiles like Dockerfile, Makefile
  const lowerName = lastSegment.toLowerCase();
  if (lowerName === "dockerfile") return "dockerfile";
  if (lowerName === "makefile" || lowerName === "gnumakefile")
    return "makefile";

  const ext = lastSegment.includes(".")
    ? lastSegment.split(".").pop()?.toLowerCase()
    : undefined;
  if (ext && ext in EXT_TO_LANGUAGE) {
    return EXT_TO_LANGUAGE[ext];
  }

  // Fallback to MIME
  if (mimeType) {
    if (mimeType === "application/json" || mimeType.endsWith("+json"))
      return "json";
    if (mimeType === "application/xml" || mimeType.endsWith("+xml"))
      return "xml";
    if (mimeType === "application/javascript") return "javascript";
    if (mimeType === "application/typescript") return "typescript";
    if (mimeType === "text/x-python") return "python";
    if (mimeType === "text/x-shellscript") return "shell";
  }

  return "plaintext";
}

interface FileInfo {
  size: number;
  mime_type: string | null;
  is_dir: boolean;
}

interface EditorRemoteState {
  language: string;
  word_wrap: boolean;
  file_path: VfsPath | null;
  display_path: string | null;
}

function Editor() {
  const [searchParams] = useSearchParams();
  const editorState = useRemoteState<EditorRemoteState>("editor");

  // Read file info from remote state, fall back to search params
  const displayPath =
    editorState?.display_path ?? searchParams.get("path") ?? "";
  const filePath: VfsPath | null =
    editorState?.file_path ??
    (searchParams.has("vfs_path")
      ? JSON.parse(searchParams.get("vfs_path")!)
      : null);

  const [content, setContent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [fileSize, setFileSize] = useState(0);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [cursorPosition, setCursorPosition] = useState({ line: 1, column: 1 });

  const editorRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  const monacoRef = useRef<Monaco | null>(null);
  const dirtyRef = useRef(false);
  dirtyRef.current = dirty;
  const suppressDirtyRef = useRef(false);

  const language = editorState?.language ?? "plaintext";
  const wordWrap = editorState?.word_wrap ? "on" : "off";

  // Detect dark mode
  const [isDark, setIsDark] = useState(
    window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (e: MediaQueryListEvent) => setIsDark(e.matches);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);

  // Apply language changes from Rust state to Monaco
  useEffect(() => {
    const model = editorRef.current?.getModel();
    if (model && monacoRef.current) {
      monacoRef.current.editor.setModelLanguage(model, language);
    }
  }, [language]);

  // Apply word wrap changes from Rust state to Monaco
  useEffect(() => {
    editorRef.current?.updateOptions({ wordWrap });
  }, [wordWrap]);

  // When content finishes loading, make editor writable and focus it
  useEffect(() => {
    if (content !== null && editorRef.current) {
      editorRef.current.updateOptions({ readOnly: false });
      editorRef.current.focus();
    }
  }, [content]);

  // Update window title with dirty indicator
  useEffect(() => {
    if (!displayPath) return;
    const title = `${dirty ? "* " : ""}${displayPath} - Editor`;
    invoke("set_window_title", { title }).catch(console.error);
  }, [displayPath, dirty]);

  // Load file when file path becomes available
  useEffect(() => {
    if (!displayPath || !filePath) return;
    setContent(null);
    setError(null);
    setDirty(false);
    setFileSize(0);

    (async () => {
      try {
        // Get file info for language detection
        const info: FileInfo = await invoke("file_details", {
          path: filePath,
        });
        setFileSize(info.size);
        const detectedLang = detectLanguage(displayPath, info.mime_type);
        invoke("set_editor_language", { language: detectedLang }).catch(
          () => {},
        );

        // Read the entire file (with size limit enforced server-side)
        const data: number[] = await invoke("read_file", {
          path: filePath,
          maxSize: MAX_FILE_SIZE,
        });
        const decoder = new TextDecoder("utf-8", { fatal: false });
        const text = decoder.decode(new Uint8Array(data));

        // Set content in Monaco if already mounted, suppressing dirty flag
        if (editorRef.current) {
          suppressDirtyRef.current = true;
          editorRef.current.setValue(text);
          suppressDirtyRef.current = false;
        }
        setContent(text);
      } catch (e: unknown) {
        const msg = String(e);
        setError(msg);
        await message(msg, { kind: "error", title: "Error" });
        getCurrentWebviewWindow().close();
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [displayPath]);

  // Save handler
  const save = useCallback(async () => {
    const ed = editorRef.current;
    if (!ed) return;

    setSaving(true);
    try {
      const text = ed.getValue();
      const encoder = new TextEncoder();
      const data = Array.from(encoder.encode(text));
      await invoke("write_file", { path: filePath, data });
      setDirty(false);
      setFileSize(data.length);
    } catch (e: unknown) {
      await message(String(e), { kind: "error", title: "Save Error" });
    } finally {
      setSaving(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [displayPath]);

  const closeEditor = useCallback(async () => {
    if (dirtyRef.current) {
      const ok = await confirm(
        "You have unsaved changes. Close without saving?",
        { title: "Unsaved Changes", kind: "warning" },
      );
      if (!ok) return;
    }
    safeCommand("destroy_window");
  }, []);

  // Intercept window close when dirty
  useEffect(() => {
    const currentWindow = getCurrentWebviewWindow();
    const unlisten = currentWindow.onCloseRequested(async (event) => {
      if (!dirtyRef.current) return;
      event.preventDefault();
      closeEditor();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [closeEditor]);

  // Refs for Monaco actions (need stable references)
  const saveRef = useRef(save);
  saveRef.current = save;
  const closeRef = useRef(closeEditor);
  closeRef.current = closeEditor;
  useEffect(() => {
    const currentWindow = getCurrentWebviewWindow();
    const unlisten = currentWindow.listen<string>("editor-action", (event) => {
      if (event.payload === "save") {
        saveRef.current();
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const handleEditorMount: OnMount = useCallback((editor, monaco) => {
    editorRef.current = editor;
    monacoRef.current = monaco;

    // Track cursor position
    editor.onDidChangeCursorPosition((e) => {
      setCursorPosition({
        line: e.position.lineNumber,
        column: e.position.column,
      });
    });

    // Track dirty state (suppressed during programmatic setValue)
    editor.onDidChangeModelContent(() => {
      if (!dirtyRef.current && !suppressDirtyRef.current) {
        setDirty(true);
      }
    });

    // Ctrl+S / Cmd+S to save
    editor.addAction({
      id: "editor-save",
      label: "Save",
      keybindings: [2048 | 49], // KeyMod.CtrlCmd | KeyCode.KeyS
      run: () => {
        saveRef.current();
      },
    });

    // Escape to close (prompts if dirty)
    editor.addAction({
      id: "editor-close",
      label: "Close",
      keybindings: [9], // KeyCode.Escape
      run: () => {
        closeRef.current();
      },
    });
  }, []);

  if (error) {
    return (
      <div className={styles.editor}>
        <div className={styles.loadingContent} />
        <div className={styles.editorStatus}>
          <span className={styles.statusError}>{error}</span>
        </div>
      </div>
    );
  }

  const ready = filePath && content !== null;

  return (
    <div className={styles.editor}>
      <div className={styles.editorContent}>
        <MonacoEditor
          defaultValue=""
          language={language}
          theme={isDark ? "vs-dark" : "vs"}
          onMount={handleEditorMount}
          options={{
            fontSize: 13,
            lineHeight: 18,
            minimap: { enabled: false },
            scrollBeyondLastLine: false,
            automaticLayout: true,
            wordWrap,
            renderWhitespace: "selection",
            readOnly: !ready,
          }}
        />
      </div>
      <div className={styles.editorStatus}>
        {ready ? (
          <>
            <span>
              {displayPath}
              {dirty ? " [Modified]" : ""}
            </span>
            <span className={styles.statusSeparator}>|</span>
            <span>{language}</span>
            <span className={styles.statusSeparator}>|</span>
            <span>
              Ln {cursorPosition.line}, Col {cursorPosition.column}
            </span>
            <span className={styles.statusSeparator}>|</span>
            <span>{formatSize(fileSize)}</span>
            {saving && (
              <>
                <span className={styles.statusSeparator}>|</span>
                <span>Saving...</span>
              </>
            )}
          </>
        ) : filePath ? (
          <span>Loading...</span>
        ) : null}
      </div>
    </div>
  );
}

export default Editor;
