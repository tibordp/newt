import { describe, expect, it } from "vitest";

import {
  formatSize,
  formatTimeBound,
  parseCount,
  parseMode,
  parseSize,
  parseTimeBound,
} from "./searchInput";

describe("parseSize", () => {
  it("reads decimal and binary prefixes", () => {
    expect(parseSize("10 MB")).toEqual({ value: 10_000_000 });
    expect(parseSize("1.5g")).toEqual({ value: 1_500_000_000 });
    expect(parseSize("512KiB")).toEqual({ value: 524_288 });
    expect(parseSize("300")).toEqual({ value: 300 });
    expect(parseSize("  ")).toEqual({ value: null });
  });

  it("rejects what isn't a size", () => {
    expect(parseSize("big")).toHaveProperty("error");
    expect(parseSize("10 iB")).toHaveProperty("error");
  });

  it("reads back what it writes", () => {
    for (const bytes of [0, 999, 1000, 1024, 10_000_000, 3 * 2 ** 30]) {
      expect(parseSize(formatSize(bytes))).toEqual({ value: bytes });
    }
  });
});

describe("parseTimeBound", () => {
  it("reads spans and dates", () => {
    expect(parseTimeBound("7d")).toEqual({
      value: { ago: { seconds: 7 * 86_400 } },
    });
    expect(parseTimeBound("2 weeks")).toEqual({
      value: { ago: { seconds: 14 * 86_400 } },
    });
    expect(parseTimeBound("2026-01-31")).toEqual({
      value: {
        date: { date: "2026-01-31", at: new Date(2026, 0, 31).getTime() },
      },
    });
  });

  it("rejects ambiguous units and impossible dates", () => {
    expect(parseTimeBound("5m")).toHaveProperty("error");
    expect(parseTimeBound("2026-02-30")).toHaveProperty("error");
  });

  it("writes spans in the largest exact unit", () => {
    expect(formatTimeBound({ ago: { seconds: 14 * 86_400 } })).toBe("2w");
    expect(formatTimeBound({ ago: { seconds: 90 } })).toBe("90s");
    expect(formatTimeBound({ ago: { seconds: 5_400 } })).toBe("90min");
  });
});

describe("parseMode and parseCount", () => {
  it("reads octal bits", () => {
    expect(parseMode("4755")).toEqual({ value: 0o4755 });
    expect(parseMode("9")).toHaveProperty("error");
  });

  it("reads positive whole numbers", () => {
    expect(parseCount("3", "Depth")).toEqual({ value: 3 });
    expect(parseCount("0", "Depth")).toHaveProperty("error");
  });
});
