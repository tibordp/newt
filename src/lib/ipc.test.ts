import { describe, it, expect } from "vitest";

// deepUpdate is not exported, so we re-implement it here for testing.
// This tests the exact logic from ipc.ts.
function deepUpdate(original: any, received: any): any {
  if (
    original === null ||
    received === null ||
    Array.isArray(original) !== Array.isArray(received) ||
    typeof original !== typeof received
  ) {
    return received;
  }

  let isChanged = false;
  let ret;
  if (Array.isArray(original)) {
    if (original.length !== received.length) {
      return received;
    }

    const result = Array(original.length);
    for (let i = 0; i < original.length; i++) {
      result[i] = deepUpdate(original[i], received[i]);
      isChanged = isChanged || result[i] !== original[i];
    }

    ret = isChanged ? result : original;
  } else if (typeof original === "object") {
    const keys = new Set([...Object.keys(original), ...Object.keys(received)]);

    const result: Record<string, any> = {};
    for (const key of keys) {
      result[key] = deepUpdate(original[key], received[key]);
      isChanged = isChanged || result[key] !== original[key];
    }
    ret = isChanged ? result : original;
  } else {
    ret = received;
  }

  return ret;
}

describe("deepUpdate", () => {
  it("returns received when original is null", () => {
    expect(deepUpdate(null, { a: 1 })).toEqual({ a: 1 });
  });

  it("returns null when received is null", () => {
    expect(deepUpdate({ a: 1 }, null)).toBeNull();
  });

  it("preserves reference when no changes", () => {
    const original = { a: 1, b: { c: 2 } };
    const received = { a: 1, b: { c: 2 } };
    const result = deepUpdate(original, received);
    expect(result).toBe(original); // same reference
    expect(result.b).toBe(original.b);
  });

  it("returns new object when value changed", () => {
    const original = { a: 1, b: 2 };
    const received = { a: 1, b: 3 };
    const result = deepUpdate(original, received);
    expect(result).not.toBe(original);
    expect(result).toEqual({ a: 1, b: 3 });
  });

  it("handles nested objects preserving unchanged refs", () => {
    const inner = { x: 10 };
    const original = { a: inner, b: { y: 20 } };
    const received = { a: { x: 10 }, b: { y: 30 } };
    const result = deepUpdate(original, received);
    expect(result.a).toBe(inner); // unchanged, same ref
    expect(result.b).not.toBe(original.b); // changed
    expect(result.b.y).toBe(30);
  });

  it("handles arrays preserving reference when unchanged", () => {
    const original = [1, 2, 3];
    const received = [1, 2, 3];
    expect(deepUpdate(original, received)).toBe(original);
  });

  it("returns received array when length differs", () => {
    const original = [1, 2];
    const received = [1, 2, 3];
    expect(deepUpdate(original, received)).toBe(received);
  });

  it("returns received array when element changed", () => {
    const original = [1, 2, 3];
    const received = [1, 99, 3];
    const result = deepUpdate(original, received);
    expect(result).not.toBe(original);
    expect(result).toEqual([1, 99, 3]);
  });

  it("handles type mismatch (array vs object)", () => {
    const result = deepUpdate([1, 2], { a: 1 });
    expect(result).toEqual({ a: 1 });
  });

  it("handles type mismatch (string vs number)", () => {
    expect(deepUpdate("hello", 42)).toBe(42);
  });

  it("handles primitive values", () => {
    expect(deepUpdate(1, 1)).toBe(1);
    expect(deepUpdate("a", "a")).toBe("a");
    expect(deepUpdate(true, false)).toBe(false);
  });

  it("handles keys added in received", () => {
    const original = { a: 1 };
    const received = { a: 1, b: 2 };
    const result = deepUpdate(original, received);
    expect(result).toEqual({ a: 1, b: 2 });
    expect(result).not.toBe(original);
  });

  it("handles keys removed in received (becomes undefined)", () => {
    const original = { a: 1, b: 2 };
    const received = { a: 1 };
    const result = deepUpdate(original, received);
    // The key "b" will be set to deepUpdate(2, undefined).
    // Since typeof 2 !== typeof undefined, returns undefined.
    expect(result.a).toBe(1);
    expect(result.b).toBeUndefined();
  });
});
