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

function Editor() {
  const [searchParams] = useSearchParams();
  const displayPath = searchParams.get("path") || "";
  const filePath: VfsPath = JSON.parse(
    searchParams.get("vfs_path") ||
      `{"vfs_id":0,"path":${JSON.stringify(displayPath)}}`,
  );

  const editorState = useRemoteState<{ language: string; word_wrap: boolean }>(
    "editor",
  );

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

  // Update window title with dirty indicator
  useEffect(() => {
    if (!displayPath) return;
    const title = `${dirty ? "* " : ""}${displayPath} - Editor`;
    invoke("set_window_title", { title }).catch(console.error);
  }, [displayPath, dirty]);

  // Load file on mount
  useEffect(() => {
    if (!displayPath) return;

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
        setContent(decoder.decode(new Uint8Array(data)));
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

  // Intercept window close when dirty
  useEffect(() => {
    const currentWindow = getCurrentWebviewWindow();
    const unlisten = currentWindow.onCloseRequested(async (event) => {
      if (!dirtyRef.current) return;
      event.preventDefault();
      const ok = await confirm(
        "You have unsaved changes. Close without saving?",
        { title: "Unsaved Changes", kind: "warning" },
      );
      if (ok) {
        safeCommand("destroy_window");
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Listen for menu action events (save)
  const saveRef = useRef(save);
  saveRef.current = save;
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

  const handleEditorMount: OnMount = useCallback(
    (editor, monaco) => {
      editorRef.current = editor;
      monacoRef.current = monaco;

      // Track cursor position
      editor.onDidChangeCursorPosition((e) => {
        setCursorPosition({
          line: e.position.lineNumber,
          column: e.position.column,
        });
      });

      // Track dirty state
      editor.onDidChangeModelContent(() => {
        if (!dirtyRef.current) {
          setDirty(true);
        }
      });

      // Ctrl+S / Cmd+S to save
      editor.addAction({
        id: "editor-save",
        label: "Save",
        keybindings: [2048 | 49], // KeyMod.CtrlCmd | KeyCode.KeyS
        run: () => {
          save();
        },
      });

      editor.focus();
    },
    [save],
  );

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

  if (content === null) {
    return (
      <div className={styles.editor}>
        <div className={styles.loadingContent}>Loading...</div>
        <div className={styles.editorStatus}>
          <span>{displayPath}</span>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.editor}>
      <div className={styles.editorContent}>
        <MonacoEditor
          defaultValue={content}
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
          }}
        />
      </div>
      <div className={styles.editorStatus}>
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
      </div>
    </div>
  );
}

export default Editor;
