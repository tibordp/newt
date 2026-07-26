import { describe, it, expect } from "vitest";

import { formatBytes } from "./size";

describe("formatBytes", () => {
  it("scales by 1000 with SI prefixes", () => {
    expect(formatBytes(0, "decimal")).toBe("0 B");
    expect(formatBytes(512, "decimal")).toBe("512 B");
    expect(formatBytes(1000, "decimal")).toBe("1 kB");
    expect(formatBytes(1_500_000, "decimal")).toBe("1.5 MB");
    expect(formatBytes(2_500_000_000, "decimal")).toBe("2.5 GB");
  });

  it("scales by 1024 with IEC prefixes", () => {
    expect(formatBytes(0, "binary")).toBe("0 B");
    expect(formatBytes(1023, "binary")).toBe("1,023 B");
    expect(formatBytes(1024, "binary")).toBe("1 KiB");
    expect(formatBytes(1536, "binary")).toBe("1.5 KiB");
    expect(formatBytes(1024 ** 3, "binary")).toBe("1 GiB");
  });

  /// A value just under 1 KiB is still bytes under binary, while decimal
  /// has already crossed into kB.
  it("keeps base and prefix in step", () => {
    expect(formatBytes(1024, "decimal")).toBe("1.02 kB");
    expect(formatBytes(1024, "binary")).toBe("1 KiB");
  });

  it("defaults to decimal", () => {
    expect(formatBytes(1_000_000)).toBe("1 MB");
  });

  it("drops trailing zeros and caps at two decimals", () => {
    expect(formatBytes(1_100_000, "decimal")).toBe("1.1 MB");
    expect(formatBytes(1_234_567, "decimal")).toBe("1.23 MB");
  });

  it("formats the number in the given locale", () => {
    expect(formatBytes(1_500_000, "decimal", "de-DE")).toBe("1,5 MB");
    expect(formatBytes(1536, "binary", "de-DE")).toBe("1,5 KiB");
  });

  it("saturates at the largest prefix", () => {
    expect(formatBytes(10 ** 21, "decimal")).toBe("1,000 EB");
  });
});
