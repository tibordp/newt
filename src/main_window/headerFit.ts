import { type RefObject, useLayoutEffect, useState } from "react";

/// Header widths below which a part gives way. Fixed rather than fitted to
/// the path, so two panes as wide look alike, and a missing part says the
/// pane is narrow, not that the path is long or the filesystem has none.
const GIT_BADGE_MIN = 480;
const FREE_SPACE_MIN = 340;
const VFS_NAME_MIN = 280;

/// What the pane header shows at its width.
export type HeaderPlan = {
  gitBadge: boolean;
  freeSpace: boolean;
  /// The filesystem selector shows its icon rather than its name.
  compactVfs: boolean;
  /// Leading breadcrumbs folded into a `⋯`, so the current folder's name is
  /// the last thing to go; never the last crumb itself.
  hiddenCrumbs: number;
};

export function headerParts(width: number) {
  return {
    gitBadge: width >= GIT_BADGE_MIN,
    freeSpace: width >= FREE_SPACE_MIN,
    compactVfs: width < VFS_NAME_MIN,
  };
}

const sum = (xs: number[]) => xs.reduce((a, b) => a + b, 0);

/// How many leading crumbs to fold for the path to fit `room`: the fewest
/// that do, else all but the last, whose name is then cut short at its end.
export function foldCrumbs(
  room: number,
  crumbs: number[],
  ellipsis: number,
): number {
  for (let hidden = 0; hidden < crumbs.length - 1; hidden++) {
    const width = sum(crumbs.slice(hidden)) + (hidden ? ellipsis : 0);
    // Laid-out widths round differently from measured ones.
    if (width + 1 <= room) return hidden;
  }
  return Math.max(crumbs.length - 1, 0);
}

const widthOf = (el: Element) => el.getBoundingClientRect().width;

/// The crumbs to fold: the room the header's other items leave the path
/// (marked `data-fit`), against each crumb's natural width from `layer`, a
/// hidden copy of them.
function measureFold(header: HTMLElement, layer: HTMLElement): number {
  const style = getComputedStyle(header);
  const gap = parseFloat(style.columnGap) || 0;
  const content =
    header.clientWidth -
    parseFloat(style.paddingLeft) -
    parseFloat(style.paddingRight);
  const others = [...header.children].filter((el) => {
    if (!(el instanceof HTMLElement) || el.dataset.fit !== undefined) {
      return false;
    }
    const { display, position } = getComputedStyle(el);
    return display !== "none" && position !== "absolute";
  });
  const all = (key: string) =>
    [...layer.querySelectorAll(`[data-measure="${key}"]`)].map(widthOf);
  return foldCrumbs(
    content - sum(others.map(widthOf)) - gap * others.length,
    all("crumb"),
    all("ellipsis")[0] ?? 0,
  );
}

/// The plan for the header, kept current as it resizes or `content`
/// (anything it shows) changes.
export function useHeaderPlan(
  header: RefObject<HTMLElement | null>,
  layer: RefObject<HTMLElement | null>,
  content: string,
): HeaderPlan {
  const [width, setWidth] = useState(Infinity);
  const [hiddenCrumbs, setHiddenCrumbs] = useState(0);
  const parts = headerParts(width);

  // The parts follow the width; the fold is then measured against them as
  // laid out, so it runs again whenever they change.
  useLayoutEffect(() => {
    const h = header.current;
    const l = layer.current;
    if (!h || !l) return;
    const refit = () => {
      setWidth(h.clientWidth);
      setHiddenCrumbs(measureFold(h, l));
    };
    refit();
    const observer = new ResizeObserver(refit);
    observer.observe(h);
    for (const el of [...h.children, ...l.querySelectorAll("[data-measure]")]) {
      observer.observe(el);
    }
    return () => observer.disconnect();
  }, [
    header,
    layer,
    content,
    parts.gitBadge,
    parts.freeSpace,
    parts.compactVfs,
  ]);
  return { ...parts, hiddenCrumbs };
}
