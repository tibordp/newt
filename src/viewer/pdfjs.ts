import workerUrl from "pdfjs-dist/legacy/build/pdf.worker.mjs?worker&url";

// pdf.js lives in chunks of its own, so a WebKit it can't run on takes down
// PDF mode, not the windows that host it. Loading starts as soon as such a
// window starts (viewer windows, prewarmed ones included, and the main window
// for Quick View), so it is ready before the first PDF.
export const pdfjs = import("pdfjs-dist/legacy/build/pdf.mjs").then(
  async (lib) => {
    lib.GlobalWorkerOptions.workerSrc = workerUrl;
    // Reads `globalThis.pdfjsLib`, which `pdf.mjs` sets: load it second.
    const viewer = await import("pdfjs-dist/legacy/web/pdf_viewer.mjs");
    return { lib, viewer };
  },
);

export type Pdfjs = Awaited<typeof pdfjs>;

// A failure is reported where a PDF is shown.
pdfjs.catch(() => {});
