import { describe, it, expect } from "vitest";
import {
  progressFraction,
  formatProgress,
  type OperationState,
} from "./OperationsPanel";

function makeOp(overrides: Partial<OperationState> = {}): OperationState {
  return {
    id: 1,
    kind: "copy",
    description: "Copying files",
    total_bytes: null,
    total_items: null,
    bytes_done: 0,
    items_done: 0,
    current_item: "",
    status: "running",
    error: null,
    issue: null,
    backgrounded: false,
    scanning_items: null,
    scanning_bytes: null,
    ...overrides,
  };
}

describe("progressFraction", () => {
  it("returns 0 when scanning", () => {
    expect(progressFraction(makeOp({ status: "scanning" }))).toBe(0);
  });

  it("returns bytes fraction when total_bytes available", () => {
    expect(progressFraction(makeOp({ total_bytes: 100, bytes_done: 50 }))).toBe(
      0.5,
    );
  });

  it("returns items fraction when no bytes info", () => {
    expect(progressFraction(makeOp({ total_items: 10, items_done: 3 }))).toBe(
      0.3,
    );
  });

  it("prefers bytes over items", () => {
    expect(
      progressFraction(
        makeOp({
          total_bytes: 200,
          bytes_done: 100,
          total_items: 10,
          items_done: 1,
        }),
      ),
    ).toBe(0.5);
  });

  it("returns 0 when no progress info", () => {
    expect(progressFraction(makeOp())).toBe(0);
  });

  it("returns 1 when complete", () => {
    expect(
      progressFraction(makeOp({ total_bytes: 100, bytes_done: 100 })),
    ).toBe(1);
  });

  it("returns 0 for zero total_bytes", () => {
    expect(progressFraction(makeOp({ total_bytes: 0 }))).toBe(0);
  });

  it("returns 0 for zero total_items", () => {
    expect(progressFraction(makeOp({ total_items: 0 }))).toBe(0);
  });
});

describe("formatProgress", () => {
  it("returns 'Scanning...' for scanning without info", () => {
    expect(formatProgress(makeOp({ status: "scanning" }))).toBe("Scanning...");
  });

  it("shows items count when scanning with items", () => {
    expect(
      formatProgress(makeOp({ status: "scanning", scanning_items: 42 })),
    ).toBe("Scanning... 42 items");
  });

  it("shows items and bytes when scanning with both", () => {
    expect(
      formatProgress(
        makeOp({
          status: "scanning",
          scanning_items: 42,
          scanning_bytes: 1024 * 1024,
        }),
      ),
    ).toBe("Scanning... 42 items, 1.0 MB");
  });

  it("shows percentage and sizes for bytes-based progress", () => {
    expect(
      formatProgress(
        makeOp({
          total_bytes: 1024 * 1024,
          bytes_done: 512 * 1024,
        }),
      ),
    ).toBe("50% (512.0 KB / 1.0 MB)");
  });

  it("shows items progress when no bytes", () => {
    expect(formatProgress(makeOp({ total_items: 100, items_done: 25 }))).toBe(
      "25/100",
    );
  });

  it("returns empty string when no progress info", () => {
    expect(formatProgress(makeOp())).toBe("");
  });
});
