import { describe, it, expect } from "vitest";

import { strftime, formatDate, formatTime, formatDateTime } from "./datetime";

// Local-time constructor so tests are timezone-independent.
const d = new Date(2026, 6, 4, 9, 5, 7); // Sat Jul 4 2026, 09:05:07

describe("strftime", () => {
  it("formats numeric date specifiers", () => {
    expect(strftime(d, "%Y-%m-%d")).toBe("2026-07-04");
    expect(strftime(d, "%d.%m.%y")).toBe("04.07.26");
    expect(strftime(d, "%e")).toBe(" 4");
    expect(strftime(d, "%j")).toBe("185");
  });

  it("formats time specifiers", () => {
    expect(strftime(d, "%H:%M:%S")).toBe("09:05:07");
    expect(strftime(d, "%I:%M %p")).toBe("09:05 AM");
    expect(strftime(new Date(2026, 6, 4, 15, 0, 0), "%I %p")).toBe("03 PM");
    expect(strftime(new Date(2026, 6, 4, 0, 30, 0), "%I %p")).toBe("12 AM");
  });

  it("passes through literals and unknown specifiers", () => {
    expect(strftime(d, "%% %Q x")).toBe("% %Q x");
  });
});

describe("format helpers", () => {
  const ms = d.getTime();

  it("uses the format when given", () => {
    expect(formatDate(ms, "%Y/%m/%d")).toBe("2026/07/04");
    expect(formatTime(ms, "%H%M")).toBe("0905");
    expect(formatDateTime(ms, "%Y-%m-%d", "%H:%M")).toBe("2026-07-04 09:05");
  });

  it("falls back to locale rendering for empty formats", () => {
    expect(formatDate(ms, "")).toBe(d.toLocaleDateString());
    expect(formatTime(ms)).toBe(d.toLocaleTimeString());
    expect(formatDateTime(ms)).toBe(d.toLocaleString());
    // Mixed: only one side has an explicit format
    expect(formatDateTime(ms, "%Y", "")).toBe(`2026 ${d.toLocaleTimeString()}`);
  });
});
