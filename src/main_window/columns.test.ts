import { describe, it, expect } from "vitest";

import {
  getTimestampState,
  getVisibleColumns,
  insertColumnKey,
  moveColumn,
  setTimestampState,
  visibleConfigKeys,
} from "./columns";

const keys = (cols: string[]) => getVisibleColumns(cols).map((c) => c.key);

describe("getVisibleColumns", () => {
  it("keeps the compound timestamp column when no time column is visible", () => {
    expect(keys(["name", "size", "modified"])).toEqual([
      "name",
      "size",
      "modified",
    ]);
  });

  it("swaps compound → date-only when the paired time column is added", () => {
    expect(keys(["name", "modified", "modified_time"])).toEqual([
      "name",
      "modified_date",
      "modified_time",
    ]);
    expect(keys(["name", "created", "created_time"])).toEqual([
      "name",
      "created_date",
      "created_time",
    ]);
  });

  it("does not swap across different timestamps", () => {
    expect(keys(["name", "modified", "accessed_time"])).toEqual([
      "name",
      "modified",
      "accessed_time",
    ]);
  });

  it("still resolves legacy explicit date-only keys", () => {
    expect(keys(["name", "modified_date", "modified_time"])).toEqual([
      "name",
      "modified_date",
      "modified_time",
    ]);
    expect(keys(["name", "modified_date"])).toEqual(["name", "modified_date"]);
  });

  it("swaps name → stem when extension is visible", () => {
    expect(keys(["name", "extension", "modified"])).toEqual([
      "stem",
      "extension",
      "modified",
    ]);
  });
});

describe("getTimestampState", () => {
  it("reports each presentation", () => {
    expect(getTimestampState(["name", "modified"], "modified")).toBe(
      "datetime",
    );
    expect(getTimestampState(["name", "modified_date"], "modified")).toBe(
      "date",
    );
    expect(getTimestampState(["modified", "modified_time"], "modified")).toBe(
      "split",
    );
    expect(
      getTimestampState(["modified_date", "modified_time"], "modified"),
    ).toBe("split");
    expect(getTimestampState(["name", "size"], "modified")).toBe("hidden");
  });

  it("is scoped to the given timestamp", () => {
    expect(getTimestampState(["modified", "accessed_time"], "modified")).toBe(
      "datetime",
    );
    expect(getTimestampState(["modified", "accessed_time"], "accessed")).toBe(
      "hidden",
    );
  });
});

describe("setTimestampState", () => {
  const base = ["name", "size", "modified", "user"];

  it("rewrites the timestamp's columns in place", () => {
    expect(setTimestampState(base, "modified", "date")).toEqual([
      "name",
      "size",
      "modified_date",
      "user",
    ]);
    expect(setTimestampState(base, "modified", "split")).toEqual([
      "name",
      "size",
      "modified_date",
      "modified_time",
      "user",
    ]);
    expect(setTimestampState(base, "modified", "hidden")).toEqual([
      "name",
      "size",
      "user",
    ]);
  });

  it("collapses split back to compound in place", () => {
    expect(
      setTimestampState(
        ["name", "modified_date", "modified_time", "user"],
        "modified",
        "datetime",
      ),
    ).toEqual(["name", "modified", "user"]);
  });

  it("inserts canonically when previously hidden", () => {
    expect(
      setTimestampState(["name", "size", "user"], "modified", "split"),
    ).toEqual(["name", "size", "modified_date", "modified_time", "user"]);
  });

  it("cleans up a stray lone time column", () => {
    expect(
      setTimestampState(["name", "modified_time"], "modified", "date"),
    ).toEqual(["name", "modified_date"]);
  });
});

describe("visibleConfigKeys", () => {
  it("is index-aligned with getVisibleColumns", () => {
    const cfg = ["name", "modified", "modified_time", "bogus", "user"];
    expect(visibleConfigKeys(cfg)).toEqual([
      "name",
      "modified",
      "modified_time",
      "user",
    ]);
    expect(getVisibleColumns(cfg).map((c) => c.key)).toEqual([
      "name",
      "modified_date",
      "modified_time",
      "user",
    ]);
  });

  it("forces name in when omitted", () => {
    expect(visibleConfigKeys(["size", "mode"])).toEqual([
      "name",
      "size",
      "mode",
    ]);
  });
});

describe("moveColumn", () => {
  const cfg = ["name", "size", "modified_date", "modified_time", "user"];

  it("moves a rendered column to an insertion boundary", () => {
    expect(moveColumn(cfg, 1, 4)).toEqual([
      "name",
      "modified_date",
      "modified_time",
      "size",
      "user",
    ]);
    expect(moveColumn(cfg, 3, 0)).toEqual([
      "modified_time",
      "name",
      "size",
      "modified_date",
      "user",
    ]);
  });

  it("moves swapped columns by their config key", () => {
    expect(moveColumn(["name", "modified", "modified_time"], 2, 0)).toEqual([
      "modified_time",
      "name",
      "modified",
    ]);
  });
});

describe("insertColumnKey", () => {
  const defaults = ["name", "size", "modified", "user", "group", "mode"];

  it("inserts at the canonical position", () => {
    expect(insertColumnKey(defaults, "extension")).toEqual([
      "name",
      "size",
      "extension",
      "modified",
      "user",
      "group",
      "mode",
    ]);
    expect(insertColumnKey(defaults, "modified_time")).toEqual([
      "name",
      "size",
      "modified",
      "modified_time",
      "user",
      "group",
      "mode",
    ]);
  });

  it("appends after everything canonically smaller", () => {
    expect(insertColumnKey(["name", "mode"], "symlink_target")).toEqual([
      "name",
      "mode",
      "symlink_target",
    ]);
  });
});
