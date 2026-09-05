export type { VfsPath } from "../lib/types";
export type {
  ExifRow,
  FileChunk,
  FileDetails as FileInfo,
} from "../lib/bindings";

export const TEXT_MIME_PREFIXES = ["text/"];
export const TEXT_MIME_TYPES = new Set([
  "application/json",
  "application/xml",
  "application/javascript",
  "application/typescript",
  "application/xhtml+xml",
  "application/x-sh",
  "application/x-csh",
  "application/x-httpd-php",
  "application/graphql",
  "application/sql",
  "application/x-yaml",
  "application/toml",
  "application/x-perl",
  "application/x-ruby",
  "application/x-python",
  "application/x-lua",
  "application/wasm",
  "application/ld+json",
  "application/manifest+json",
  "application/schema+json",
  "image/svg+xml",
]);

export function isTextMime(mime: string | null): boolean {
  if (!mime) return false;
  if (TEXT_MIME_PREFIXES.some((p) => mime.startsWith(p))) return true;
  if (TEXT_MIME_TYPES.has(mime)) return true;
  // Catch-all for +xml, +json suffixes
  if (mime.endsWith("+xml") || mime.endsWith("+json")) return true;
  return false;
}

export function isImageMime(mime: string | null): boolean {
  if (!mime) return false;
  return mime.startsWith("image/");
}

export function isAudioMime(mime: string | null): boolean {
  if (!mime) return false;
  return mime.startsWith("audio/");
}

export function isVideoMime(mime: string | null): boolean {
  if (!mime) return false;
  return mime.startsWith("video/");
}

export function isPdfMime(mime: string | null): boolean {
  return mime === "application/pdf";
}

export type { ViewerMode } from "../lib/bindings";
import type { ViewerMode } from "../lib/bindings";

export function detectAutoMode(mime: string | null): ViewerMode {
  if (isVideoMime(mime)) return "video";
  if (isAudioMime(mime)) return "audio";
  if (isPdfMime(mime)) return "pdf";
  if (isImageMime(mime)) return "image";
  if (isTextMime(mime)) return "text";
  return "hex";
}

export function buildFileUrl(
  fileServerBase: string,
  vfsId: number,
  path: string,
): string {
  const url = new URL(`${fileServerBase}/${vfsId}`);
  url.searchParams.set("path", path);
  return url.toString();
}

export const CHUNK_SIZE = 128 * 1024;
export const HEX_BYTES_PER_ROW = 16;
export const MAX_SCROLL_HEIGHT = 16_000_000; // stay under browser element height limit

export function formatHexOffset(offset: number): string {
  return offset.toString(16).padStart(8, "0").toUpperCase();
}

export function hexByte(b: number): string {
  return b.toString(16).padStart(2, "0").toUpperCase();
}

export function printableAscii(b: number): string {
  return b >= 0x20 && b <= 0x7e ? String.fromCharCode(b) : ".";
}

// --- Shared helpers ---

/**
 * LRU chunk cache. Uses Map insertion order for recency tracking:
 * delete + re-insert on access moves the key to the end (most recent).
 * Evicts the oldest entry (first key) when capacity is exceeded.
 */
export const MAX_CACHED_CHUNKS = 32; // 32 × 128 KB = 4 MB

export class LruChunkCache {
  private map = new Map<number, Uint8Array>();
  private maxSize: number;

  constructor(maxSize: number) {
    this.maxSize = maxSize;
  }

  get(key: number): Uint8Array | undefined {
    const value = this.map.get(key);
    if (value !== undefined) {
      // Move to end (most recently used)
      this.map.delete(key);
      this.map.set(key, value);
    }
    return value;
  }

  has(key: number): boolean {
    return this.map.has(key);
  }

  set(key: number, value: Uint8Array): void {
    if (this.map.has(key)) {
      this.map.delete(key);
    }
    this.map.set(key, value);
    while (this.map.size > this.maxSize) {
      const oldest = this.map.keys().next().value;
      if (oldest !== undefined) this.map.delete(oldest);
    }
  }

  clear(): void {
    this.map.clear();
  }
}

/**
 * Collect a contiguous byte range from the chunk cache into a Uint8Array.
 * Missing chunks produce zero-filled gaps (shouldn't happen in practice
 * since callers ensure relevant chunks are loaded first).
 */
export function collectBytes(
  chunkCache: LruChunkCache,
  startByte: number,
  endByte: number,
): Uint8Array {
  const result = new Uint8Array(endByte - startByte);
  let pos = 0;
  let offset = startByte;

  while (offset < endByte) {
    const ci = Math.floor(offset / CHUNK_SIZE);
    const chunk = chunkCache.get(ci);
    const chunkStart = ci * CHUNK_SIZE;
    const localStart = offset - chunkStart;

    if (!chunk) {
      const nextBoundary = Math.min((ci + 1) * CHUNK_SIZE, endByte);
      pos += nextBoundary - offset;
      offset = nextBoundary;
      continue;
    }

    const available = Math.min(chunk.length - localStart, endByte - offset);
    if (available <= 0) break;

    result.set(chunk.subarray(localStart, localStart + available), pos);
    pos += available;
    offset += available;
  }

  return result.subarray(0, pos);
}

// --- Text encodings ---

/** Leading bytes the text viewer hands to the backend encoding sniffer. */
export const SNIFF_PREFIX_LEN = 64 * 1024;

export type EncodingKind = "utf8" | "utf16le" | "utf16be" | "single" | "multi";

/** How newlines are found in the byte stream for an encoding. */
export type NewlineScan = "byte" | "utf16le" | "utf16be";

// Legacy multibyte encodings in the viewer's catalogue: bytes per character
// vary and the browser has no encoder for them.
const MULTIBYTE_LEGACY = new Set([
  "shift_jis",
  "euc-jp",
  "iso-2022-jp",
  "gbk",
  "gb18030",
  "big5",
  "euc-kr",
]);

export function encodingKind(encoding: string): EncodingKind {
  const e = encoding.toLowerCase();
  if (e === "utf-8") return "utf8";
  if (e === "utf-16le") return "utf16le";
  if (e === "utf-16be") return "utf16be";
  return MULTIBYTE_LEGACY.has(e) ? "multi" : "single";
}

export function newlineScan(encoding: string): NewlineScan {
  const kind = encodingKind(encoding);
  return kind === "utf16le" || kind === "utf16be" ? kind : "byte";
}

/**
 * Decoder for slices taken mid-file. A BOM-shaped sequence inside the file
 * is data; the real BOM is skipped by offset (line 0 starts after it).
 */
export function makeDecoder(encoding: string): TextDecoder {
  return new TextDecoder(encoding, { fatal: false, ignoreBOM: true });
}

/**
 * Append the absolute byte offsets of the line starts found in `chunk` from
 * absolute offset `from` onwards. UTF-16 newlines are the code unit 0x000A,
 * so `from` must be code-unit aligned; in every other catalogue encoding
 * 0x0A never occurs inside a multibyte sequence and a byte scan is exact.
 */
export function scanLineStarts(
  chunk: Uint8Array,
  chunkStart: number,
  from: number,
  scan: NewlineScan,
  out: number[],
): void {
  const start = from - chunkStart;
  if (scan === "byte") {
    for (let i = start; i < chunk.length; i++) {
      if (chunk[i] === 0x0a) out.push(chunkStart + i + 1);
    }
    return;
  }
  const [lo, hi] = scan === "utf16le" ? [0x0a, 0] : [0, 0x0a];
  for (let i = start; i + 1 < chunk.length; i += 2) {
    if (chunk[i] === lo && chunk[i + 1] === hi) out.push(chunkStart + i + 2);
  }
}

/**
 * Number of leading bytes of `bytes` that decode to `col` UTF-16 code units
 * (rounded up to a character boundary). `bytes` must start on a character
 * boundary.
 */
export function colToByteLength(
  bytes: Uint8Array,
  encoding: string,
  col: number,
): number {
  if (col <= 0) return 0;
  switch (encodingKind(encoding)) {
    case "single":
      return Math.min(col, bytes.length);
    case "utf16le":
    case "utf16be":
      return Math.min(col * 2, bytes.length);
    case "utf8": {
      // Count code units from lead bytes: 4-byte sequences are a surrogate
      // pair, anything else is one unit — including a stray continuation
      // byte, which decodes to one U+FFFD.
      let units = 0;
      let pending = 0;
      for (let i = 0; i < bytes.length; i++) {
        const b = bytes[i];
        if ((b & 0xc0) === 0x80 && pending > 0) {
          pending--;
          continue;
        }
        if (units >= col) return i;
        pending = b >= 0xf0 ? 3 : b >= 0xe0 ? 2 : b >= 0xc0 ? 1 : 0;
        units += b >= 0xf0 ? 2 : 1;
      }
      return bytes.length;
    }
    case "multi": {
      // No encoder available: stream bytes one at a time and stop once the
      // decoder has emitted enough units.
      const decoder = makeDecoder(encoding);
      let units = 0;
      for (let i = 0; i < bytes.length; i++) {
        units += decoder.decode(bytes.subarray(i, i + 1), {
          stream: true,
        }).length;
        if (units >= col) return i + 1;
      }
      return bytes.length;
    }
  }
}
