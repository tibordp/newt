import { describe, it, expect } from "vitest";
import {
  isTextMime,
  isImageMime,
  isAudioMime,
  isVideoMime,
  isPdfMime,
  detectAutoMode,
  buildFileUrl,
  formatHexOffset,
  hexByte,
  printableAscii,
  LruChunkCache,
  collectBytes,
  CHUNK_SIZE,
  scanLineStarts,
  colToByteLength,
  newlineScan,
} from "./helpers";

describe("buildFileUrl", () => {
  it("passes the VFS path as an opaque query parameter", () => {
    expect(
      buildFileUrl("http://localhost:1234/token", 7, "/photos/a.png"),
    ).toBe("http://localhost:1234/token/7?path=%2Fphotos%2Fa.png");
  });

  it("encodes URL syntax and percent-encoded-looking names", () => {
    expect(
      buildFileUrl("http://localhost:1234/token", 7, "/scan #1%2F?.pdf"),
    ).toBe("http://localhost:1234/token/7?path=%2Fscan+%231%252F%3F.pdf");
  });

  it("encodes unicode as UTF-8", () => {
    expect(buildFileUrl("http://localhost:1234/token", 7, "/café/猫.jpg")).toBe(
      "http://localhost:1234/token/7?path=%2Fcaf%C3%A9%2F%E7%8C%AB.jpg",
    );
  });
});

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

describe("scanLineStarts", () => {
  it("byte scan records the offset after each 0x0A", () => {
    const out: number[] = [];
    scanLineStarts(
      new Uint8Array([0x61, 0x0a, 0x62, 0x0a, 0x0a]),
      100,
      100,
      "byte",
      out,
    );
    expect(out).toEqual([102, 104, 105]);
  });

  it("resumes mid-chunk", () => {
    const out: number[] = [];
    scanLineStarts(new Uint8Array([0x0a, 0x0a, 0x0a]), 0, 2, "byte", out);
    expect(out).toEqual([3]);
  });

  it("UTF-16 scans aligned code units only", () => {
    // "a\n␊" in UTF-16LE: 0x000A is a newline, U+240A is not, and its
    // 0x0A byte sits at an even offset.
    const le = new Uint8Array([0x61, 0, 0x0a, 0, 0x0a, 0x24]);
    const out: number[] = [];
    scanLineStarts(le, 0, 0, "utf16le", out);
    expect(out).toEqual([4]);

    const be = new Uint8Array([0, 0x61, 0, 0x0a, 0x24, 0x0a]);
    const outBe: number[] = [];
    scanLineStarts(be, 0, 0, "utf16be", outBe);
    expect(outBe).toEqual([4]);
  });

  it("UTF-16 honours the BOM offset for alignment", () => {
    // BOM, then "\n" — scanning from 2 keeps the pairs aligned.
    const le = new Uint8Array([0xff, 0xfe, 0x0a, 0, 0x61, 0]);
    const out: number[] = [];
    scanLineStarts(le, 0, 2, "utf16le", out);
    expect(out).toEqual([4]);
  });
});

describe("colToByteLength", () => {
  const utf8 = (s: string) => new TextEncoder().encode(s);

  it("UTF-8 counts multibyte characters and surrogate pairs", () => {
    const bytes = utf8("aé😀b");
    expect(colToByteLength(bytes, "UTF-8", 0)).toBe(0);
    expect(colToByteLength(bytes, "UTF-8", 1)).toBe(1);
    expect(colToByteLength(bytes, "UTF-8", 2)).toBe(3);
    expect(colToByteLength(bytes, "UTF-8", 4)).toBe(7);
    expect(colToByteLength(bytes, "UTF-8", 5)).toBe(8);
    expect(colToByteLength(bytes, "UTF-8", 99)).toBe(8);
  });

  it("UTF-8 rounds a column inside a surrogate pair up to its end", () => {
    expect(colToByteLength(utf8("😀"), "UTF-8", 1)).toBe(4);
  });

  it("UTF-8 counts a stray byte as one unit", () => {
    expect(colToByteLength(new Uint8Array([0x80, 0x61]), "UTF-8", 1)).toBe(1);
    expect(colToByteLength(new Uint8Array([0x80, 0x61]), "UTF-8", 2)).toBe(2);
  });

  it("single-byte encodings map 1:1", () => {
    expect(
      colToByteLength(new Uint8Array([0xe9, 0x61, 0x62]), "windows-1252", 2),
    ).toBe(2);
    expect(colToByteLength(new Uint8Array([0xe9]), "windows-1252", 5)).toBe(1);
  });

  it("UTF-16 maps two bytes per unit", () => {
    expect(colToByteLength(new Uint8Array(8), "UTF-16LE", 3)).toBe(6);
    expect(colToByteLength(new Uint8Array(4), "UTF-16BE", 3)).toBe(4);
  });

  it("legacy multibyte encodings stream to a character boundary", () => {
    // Shift_JIS "a日b": 61 93FA 62
    const bytes = new Uint8Array([0x61, 0x93, 0xfa, 0x62]);
    expect(colToByteLength(bytes, "Shift_JIS", 1)).toBe(1);
    expect(colToByteLength(bytes, "Shift_JIS", 2)).toBe(3);
    expect(colToByteLength(bytes, "Shift_JIS", 3)).toBe(4);
  });
});

describe("newlineScan", () => {
  it("only UTF-16 needs a code-unit scan", () => {
    expect(newlineScan("UTF-8")).toBe("byte");
    expect(newlineScan("Shift_JIS")).toBe("byte");
    expect(newlineScan("windows-1251")).toBe("byte");
    expect(newlineScan("UTF-16LE")).toBe("utf16le");
    expect(newlineScan("UTF-16BE")).toBe("utf16be");
  });
});
