import { describe, it, expect } from "vitest";

import { foldCrumbs, headerParts } from "./headerFit";

describe("headerParts", () => {
  it("drops the git badge, then free space, then the VFS name", () => {
    expect(headerParts(800)).toEqual({
      gitBadge: true,
      freeSpace: true,
      compactVfs: false,
    });
    expect(headerParts(400)).toEqual({
      gitBadge: false,
      freeSpace: true,
      compactVfs: false,
    });
    expect(headerParts(300)).toEqual({
      gitBadge: false,
      freeSpace: false,
      compactVfs: false,
    });
    expect(headerParts(200)).toEqual({
      gitBadge: false,
      freeSpace: false,
      compactVfs: true,
    });
  });
});

describe("foldCrumbs", () => {
  // `/`, `Users/`, `demo/`, `src/`, `newt`.
  const crumbs = [10, 45, 40, 30, 40];
  const ellipsis = 15;

  it("folds nothing when the path fits", () => {
    expect(foldCrumbs(166, crumbs, ellipsis)).toBe(0);
  });

  it("folds the fewest leading crumbs that make it fit", () => {
    // Folding `/` alone saves nothing once the `…` takes its place.
    expect(foldCrumbs(165, crumbs, ellipsis)).toBe(2);
    expect(foldCrumbs(126, crumbs, ellipsis)).toBe(2);
    expect(foldCrumbs(125, crumbs, ellipsis)).toBe(3);
  });

  it("keeps the last crumb, however narrow", () => {
    expect(foldCrumbs(0, crumbs, ellipsis)).toBe(4);
    expect(foldCrumbs(0, [40], ellipsis)).toBe(0);
    expect(foldCrumbs(0, [], ellipsis)).toBe(0);
  });
});
