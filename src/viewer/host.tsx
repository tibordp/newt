import { createContext, useContext, useEffect, useRef } from "react";

import {
  commands,
  type Result,
  type TableDelimiter,
  type VfsPath,
} from "../lib/bindings";
import type { ViewerMode } from "./helpers";

/// Where the viewer is rendered: its own window, or Quick View in the main
/// window. The viewer's reports and mode changes go to whichever holds its
/// state.
export type ViewerHost = {
  /// Quick View: the viewer takes neither focus nor keys, which stay with
  /// the file list.
  embedded: boolean;
  setMode: (mode: ViewerMode) => Promise<Result<null, string>>;
  sniffEncoding: (
    prefix: number[],
    eof: boolean,
  ) => Promise<Result<null, string>>;
  reportTableDetection: (
    delimiter: TableDelimiter,
    header: boolean,
  ) => Promise<Result<null, string>>;
  /// A relative link followed from rendered Markdown.
  openFile: (path: VfsPath) => Promise<Result<null, string>>;
  /// Quick View: the shown viewer's copy of its own selection (text, hex,
  /// image region, table cells), for the copy shortcut. Returns whether
  /// there was a selection to copy.
  copySelection: { current: (() => boolean) | null };
};

const windowHost: ViewerHost = {
  embedded: false,
  setMode: commands.setViewerMode,
  sniffEncoding: commands.sniffViewerEncoding,
  reportTableDetection: commands.reportTableDetection,
  openFile: commands.openInViewer,
  copySelection: { current: null },
};

/// Quick View's host, bound to the file it shows so that reports about an
/// earlier file are dropped rather than applied to this one.
export function previewHost(path: VfsPath): ViewerHost {
  return {
    embedded: true,
    setMode: (mode) => commands.setPreviewMode(path, mode),
    sniffEncoding: (prefix, eof) =>
      commands.sniffPreviewEncoding(path, prefix, eof),
    reportTableDetection: (delimiter, header) =>
      commands.reportPreviewTableDetection(path, delimiter, header),
    openFile: commands.openViewer,
    copySelection: { current: null },
  };
}

export const ViewerHostContext = createContext<ViewerHost>(windowHost);

export const useViewerHost = () => useContext(ViewerHostContext);

/// Offer this viewer's selection to Quick View's copy shortcut. `copy`
/// copies it and returns whether there was a selection; a caret or cursor
/// alone is not one, and leaves the shortcut to copy the file's path.
export function useEmbeddedCopy(copy: () => boolean) {
  const host = useViewerHost();
  const copyRef = useRef(copy);
  copyRef.current = copy;
  useEffect(() => {
    if (!host.embedded) return;
    const slot = host.copySelection;
    const handler = () => copyRef.current();
    slot.current = handler;
    return () => {
      if (slot.current === handler) slot.current = null;
    };
  }, [host]);
}
