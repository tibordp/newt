export type { VfsPath } from "../lib/types";

export interface FileInfo {
  size: number;
  mime_type: string | null;
  is_dir: boolean;
  is_symlink: boolean;
  symlink_target: string | null;
  user: unknown;
  group: unknown;
  mode: unknown;
  modified: number | null;
  accessed: number | null;
  created: number | null;
}

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

export type ViewerMode = "text" | "hex" | "image" | "audio" | "video" | "pdf";

export function detectAutoMode(mime: string | null): ViewerMode {
  if (isVideoMime(mime)) return "video";
  if (isAudioMime(mime)) return "audio";
  if (isPdfMime(mime)) return "pdf";
  if (isImageMime(mime)) return "image";
  if (isTextMime(mime)) return "text";
  return "hex";
}

export interface FileChunk {
  data: number[];
  offset: number;
  total_size: number;
}

export const CHUNK_SIZE = 128 * 1024;
export const HEX_BYTES_PER_ROW = 16;
export const MAX_SCROLL_HEIGHT = 16_000_000; // stay under browser element height limit

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

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
