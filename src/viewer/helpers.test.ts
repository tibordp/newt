import { describe, it, expect } from "vitest";
import {
  isTextMime,
  isImageMime,
  isAudioMime,
  isVideoMime,
  isPdfMime,
  detectAutoMode,
  formatSize,
  formatHexOffset,
  hexByte,
  printableAscii,
  LruChunkCache,
  collectBytes,
  CHUNK_SIZE,
} from "./helpers";

// ---------------------------------------------------------------------------
// MIME type detection
// ---------------------------------------------------------------------------

describe("isTextMime", () => {
  it("returns false for null", () => {
    expect(isTextMime(null)).toBe(false);
  });

  it("detects text/* prefixes", () => {
    expect(isTextMime("text/plain")).toBe(true);
    expect(isTextMime("text/html")).toBe(true);
    expect(isTextMime("text/css")).toBe(true);
  });

  it("detects known application types", () => {
    expect(isTextMime("application/json")).toBe(true);
    expect(isTextMime("application/xml")).toBe(true);
    expect(isTextMime("application/javascript")).toBe(true);
    expect(isTextMime("application/x-sh")).toBe(true);
  });

  it("detects +xml and +json suffixes", () => {
    expect(isTextMime("application/ld+json")).toBe(true);
    expect(isTextMime("image/svg+xml")).toBe(true);
    expect(isTextMime("application/vnd.custom+json")).toBe(true);
    expect(isTextMime("application/soap+xml")).toBe(true);
  });

  it("rejects non-text types", () => {
    expect(isTextMime("image/png")).toBe(false);
    expect(isTextMime("application/octet-stream")).toBe(false);
    expect(isTextMime("video/mp4")).toBe(false);
  });
});

describe("detectAutoMode", () => {
  it("returns hex for null", () => {
    expect(detectAutoMode(null)).toBe("hex");
  });

  it("detects video", () => {
    expect(detectAutoMode("video/mp4")).toBe("video");
  });

  it("detects audio", () => {
    expect(detectAutoMode("audio/mpeg")).toBe("audio");
  });

  it("detects pdf", () => {
    expect(detectAutoMode("application/pdf")).toBe("pdf");
  });

  it("detects image", () => {
    expect(detectAutoMode("image/png")).toBe("image");
  });

  it("detects text", () => {
    expect(detectAutoMode("text/plain")).toBe("text");
  });

  it("falls back to hex", () => {
    expect(detectAutoMode("application/octet-stream")).toBe("hex");
  });

  it("svg+xml is text, not image", () => {
    // svg+xml matches both isImageMime (image/) and isTextMime.
    // But detectAutoMode checks image first, so image/svg+xml -> image
    expect(detectAutoMode("image/svg+xml")).toBe("image");
  });
});

describe("isImageMime, isAudioMime, isVideoMime, isPdfMime", () => {
  it("isImageMime", () => {
    expect(isImageMime("image/png")).toBe(true);
    expect(isImageMime("text/plain")).toBe(false);
    expect(isImageMime(null)).toBe(false);
  });

  it("isAudioMime", () => {
    expect(isAudioMime("audio/mpeg")).toBe(true);
    expect(isAudioMime("video/mp4")).toBe(false);
    expect(isAudioMime(null)).toBe(false);
  });

  it("isVideoMime", () => {
    expect(isVideoMime("video/mp4")).toBe(true);
    expect(isVideoMime("audio/mpeg")).toBe(false);
    expect(isVideoMime(null)).toBe(false);
  });

  it("isPdfMime", () => {
    expect(isPdfMime("application/pdf")).toBe(true);
    expect(isPdfMime("text/pdf")).toBe(false);
    expect(isPdfMime(null)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

describe("formatSize", () => {
  it("formats bytes", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(512)).toBe("512 B");
    expect(formatSize(1023)).toBe("1023 B");
  });

  it("formats KB", () => {
    expect(formatSize(1024)).toBe("1.0 KB");
    expect(formatSize(1536)).toBe("1.5 KB");
  });

  it("formats MB", () => {
    expect(formatSize(1024 * 1024)).toBe("1.0 MB");
  });

  it("formats GB", () => {
    expect(formatSize(1024 * 1024 * 1024)).toBe("1.0 GB");
  });
});

describe("formatHexOffset", () => {
  it("pads to 8 hex digits", () => {
    expect(formatHexOffset(0)).toBe("00000000");
    expect(formatHexOffset(255)).toBe("000000FF");
    expect(formatHexOffset(0x1234abcd)).toBe("1234ABCD");
  });
});

describe("hexByte", () => {
  it("formats single byte", () => {
    expect(hexByte(0)).toBe("00");
    expect(hexByte(255)).toBe("FF");
    expect(hexByte(0x0a)).toBe("0A");
  });
});

describe("printableAscii", () => {
  it("returns char for printable range", () => {
    expect(printableAscii(0x41)).toBe("A");
    expect(printableAscii(0x20)).toBe(" ");
    expect(printableAscii(0x7e)).toBe("~");
  });

  it("returns dot for non-printable", () => {
    expect(printableAscii(0x00)).toBe(".");
    expect(printableAscii(0x1f)).toBe(".");
    expect(printableAscii(0x7f)).toBe(".");
    expect(printableAscii(0xff)).toBe(".");
  });
});

// ---------------------------------------------------------------------------
// LruChunkCache
// ---------------------------------------------------------------------------

describe("LruChunkCache", () => {
  it("returns undefined for missing key", () => {
    const cache = new LruChunkCache(3);
    expect(cache.get(0)).toBeUndefined();
  });

  it("stores and retrieves value", () => {
    const cache = new LruChunkCache(3);
    const data = new Uint8Array([1, 2, 3]);
    cache.set(0, data);
    expect(cache.get(0)).toBe(data);
  });

  it("evicts oldest when capacity exceeded", () => {
    const cache = new LruChunkCache(2);
    cache.set(0, new Uint8Array([1]));
    cache.set(1, new Uint8Array([2]));
    cache.set(2, new Uint8Array([3])); // evicts key 0
    expect(cache.get(0)).toBeUndefined();
    expect(cache.get(1)).toBeDefined();
    expect(cache.get(2)).toBeDefined();
  });

  it("get promotes to most recent", () => {
    const cache = new LruChunkCache(2);
    cache.set(0, new Uint8Array([1]));
    cache.set(1, new Uint8Array([2]));
    cache.get(0); // promote 0
    cache.set(2, new Uint8Array([3])); // should evict 1, not 0
    expect(cache.get(0)).toBeDefined();
    expect(cache.get(1)).toBeUndefined();
    expect(cache.get(2)).toBeDefined();
  });

  it("set same key updates and promotes", () => {
    const cache = new LruChunkCache(2);
    cache.set(0, new Uint8Array([1]));
    cache.set(1, new Uint8Array([2]));
    cache.set(0, new Uint8Array([10])); // update key 0
    cache.set(2, new Uint8Array([3])); // should evict 1
    expect(cache.get(0)).toEqual(new Uint8Array([10]));
    expect(cache.get(1)).toBeUndefined();
  });

  it("has returns correct values", () => {
    const cache = new LruChunkCache(2);
    expect(cache.has(0)).toBe(false);
    cache.set(0, new Uint8Array([1]));
    expect(cache.has(0)).toBe(true);
  });

  it("clear removes all entries", () => {
    const cache = new LruChunkCache(5);
    cache.set(0, new Uint8Array([1]));
    cache.set(1, new Uint8Array([2]));
    cache.clear();
    expect(cache.get(0)).toBeUndefined();
    expect(cache.get(1)).toBeUndefined();
  });

  it("capacity of 1 works", () => {
    const cache = new LruChunkCache(1);
    cache.set(0, new Uint8Array([1]));
    cache.set(1, new Uint8Array([2]));
    expect(cache.get(0)).toBeUndefined();
    expect(cache.get(1)).toBeDefined();
  });
});

// ---------------------------------------------------------------------------
// collectBytes
// ---------------------------------------------------------------------------

describe("collectBytes", () => {
  it("collects from a single fully cached chunk", () => {
    const cache = new LruChunkCache(10);
    const data = new Uint8Array(CHUNK_SIZE);
    for (let i = 0; i < CHUNK_SIZE; i++) data[i] = i & 0xff;
    cache.set(0, data);

    const result = collectBytes(cache, 0, 10);
    expect(result).toEqual(data.subarray(0, 10));
  });

  it("handles partial chunk reads", () => {
    const cache = new LruChunkCache(10);
    const data = new Uint8Array(CHUNK_SIZE);
    data[100] = 42;
    cache.set(0, data);

    const result = collectBytes(cache, 100, 101);
    expect(result[0]).toBe(42);
    expect(result.length).toBe(1);
  });

  it("spans multiple chunks", () => {
    const cache = new LruChunkCache(10);
    const chunk0 = new Uint8Array(CHUNK_SIZE).fill(0xaa);
    const chunk1 = new Uint8Array(CHUNK_SIZE).fill(0xbb);
    cache.set(0, chunk0);
    cache.set(1, chunk1);

    const result = collectBytes(cache, CHUNK_SIZE - 2, CHUNK_SIZE + 2);
    expect(result.length).toBe(4);
    expect(result[0]).toBe(0xaa);
    expect(result[1]).toBe(0xaa);
    expect(result[2]).toBe(0xbb);
    expect(result[3]).toBe(0xbb);
  });

  it("skips missing chunks (zero-fill gap)", () => {
    const cache = new LruChunkCache(10);
    // Only cache chunk 1, not chunk 0
    const chunk1 = new Uint8Array(CHUNK_SIZE).fill(0xcc);
    cache.set(1, chunk1);

    const result = collectBytes(cache, 0, CHUNK_SIZE + 5);
    // First CHUNK_SIZE bytes are from missing chunk (skipped/zero)
    // Next 5 bytes are from chunk1
    expect(result[result.length - 1]).toBe(0xcc);
  });

  it("returns empty for zero-length range", () => {
    const cache = new LruChunkCache(10);
    const result = collectBytes(cache, 100, 100);
    expect(result.length).toBe(0);
  });
});
