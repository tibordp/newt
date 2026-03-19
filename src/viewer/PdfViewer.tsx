import React, { useCallback, useEffect, useRef, useState } from "react";

import * as pdfjsLib from "pdfjs-dist";
import {
  PDFViewer as PDFJSViewer,
  EventBus,
  PDFLinkService,
} from "pdfjs-dist/web/pdf_viewer.mjs";
import "pdfjs-dist/web/pdf_viewer.css";
import pdfjsWorkerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";

import styles from "./Viewer.module.scss";
import { formatSize, type ViewerMode } from "./helpers";
import { ModeToggle } from "./ModeToggle";

pdfjsLib.GlobalWorkerOptions.workerSrc = pdfjsWorkerUrl;

export interface PdfViewerProps {
  filePath: string;
  fileUrl: string;
  fileSize: number;
  autoMode: ViewerMode;
}

export function PdfViewer({
  filePath,
  fileUrl,
  fileSize,
  autoMode,
}: PdfViewerProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewerInstanceRef = useRef<PDFJSViewer | null>(null);
  const eventBusRef = useRef<EventBus | null>(null);

  const [numPages, setNumPages] = useState(0);
  const [currentPage, setCurrentPage] = useState(1);
  const [scale, setScale] = useState(0);
  const [pdfError, setPdfError] = useState<string | null>(null);

  // Initialize PDFViewer once
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const eventBus = new EventBus();
    eventBusRef.current = eventBus;

    const linkService = new PDFLinkService({ eventBus });

    const viewer = new PDFJSViewer({
      container,
      eventBus,
      linkService,
      textLayerMode: 1,
      annotationMode: 2,
      removePageBorders: false,
    });
    linkService.setViewer(viewer);
    viewerInstanceRef.current = viewer;

    eventBus.on("pagechanging", (evt: { pageNumber: number }) => {
      setCurrentPage(evt.pageNumber);
    });

    eventBus.on("scalechanging", (evt: { scale: number }) => {
      setScale(evt.scale);
    });

    container.focus();

    return () => {
      viewer.cleanup();
      viewerInstanceRef.current = null;
      eventBusRef.current = null;
    };
  }, []);

  // Load PDF document when fileUrl changes
  useEffect(() => {
    const viewer = viewerInstanceRef.current;
    if (!viewer) return;

    let cancelled = false;
    const loadingTask = pdfjsLib.getDocument(fileUrl);

    loadingTask.promise.then(
      (doc) => {
        if (cancelled) {
          doc.destroy();
          return;
        }
        viewer.setDocument(doc);
        setNumPages(doc.numPages);
        setPdfError(null);
      },
      (err) => {
        if (!cancelled) {
          setPdfError(err?.message || "Failed to load PDF");
        }
      },
    );

    return () => {
      cancelled = true;
      loadingTask.destroy();
    };
  }, [fileUrl]);

  const zoomIn = useCallback(() => {
    const v = viewerInstanceRef.current;
    if (v) v.increaseScale();
  }, []);

  const zoomOut = useCallback(() => {
    const v = viewerInstanceRef.current;
    if (v) v.decreaseScale();
  }, []);

  const zoomReset = useCallback(() => {
    const v = viewerInstanceRef.current;
    if (v) v.currentScaleValue = "auto";
  }, []);

  const goToPage = useCallback((page: number) => {
    const v = viewerInstanceRef.current;
    if (v && page >= 1 && page <= v.pagesCount) {
      v.currentPageNumber = page;
    }
  }, []);

  // Keyboard shortcuts for zoom (Ctrl+/-, Ctrl+0)
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && (e.key === "=" || e.key === "+")) {
        e.preventDefault();
        zoomIn();
      } else if ((e.ctrlKey || e.metaKey) && e.key === "-") {
        e.preventDefault();
        zoomOut();
      } else if ((e.ctrlKey || e.metaKey) && e.key === "0") {
        e.preventDefault();
        zoomReset();
      }
    },
    [zoomIn, zoomOut, zoomReset],
  );

  return (
    <div className={styles.viewer} onKeyDown={handleKeyDown}>
      {pdfError ? (
        <div className={styles.mediaContent}>
          <div className={styles.imageErrorMessage}>{pdfError}</div>
        </div>
      ) : (
        <>
          <div className={styles.pdfToolbar}>
            <button
              className={styles.pdfToolbarBtn}
              onClick={() => goToPage(currentPage - 1)}
              disabled={currentPage <= 1}
              title="Previous page"
            >
              &#x25B2;
            </button>
            <span className={styles.pdfPageInfo}>
              {currentPage} / {numPages || "–"}
            </span>
            <button
              className={styles.pdfToolbarBtn}
              onClick={() => goToPage(currentPage + 1)}
              disabled={currentPage >= numPages}
              title="Next page"
            >
              &#x25BC;
            </button>
            <span className={styles.pdfToolbarSep} />
            <button
              className={styles.pdfToolbarBtn}
              onClick={zoomOut}
              title="Zoom out (Ctrl+-)"
            >
              −
            </button>
            <span className={styles.pdfPageInfo}>
              {scale ? `${Math.round(scale * 100)}%` : "–"}
            </span>
            <button
              className={styles.pdfToolbarBtn}
              onClick={zoomIn}
              title="Zoom in (Ctrl+=)"
            >
              +
            </button>
            <button
              className={styles.pdfToolbarBtn}
              onClick={zoomReset}
              title="Reset zoom (Ctrl+0)"
            >
              Fit
            </button>
          </div>
          <div className={styles.pdfContainerWrapper}>
            <div
              ref={containerRef}
              className={styles.pdfContainer}
              tabIndex={0}
            >
              <div id="viewer" className="pdfViewer" />
            </div>
          </div>
        </>
      )}
      <div
        className={styles.viewerStatus}
        onContextMenu={(e) => e.preventDefault()}
      >
        <span className={styles.statusText}>
          <span>{filePath}</span>
          <span className={styles.statusSeparator}>|</span>
          <span>PDF</span>
          <span className={styles.statusSeparator}>|</span>
          <span>{formatSize(fileSize)}</span>
        </span>
        <ModeToggle currentMode="pdf" autoMode={autoMode} />
      </div>
    </div>
  );
}
