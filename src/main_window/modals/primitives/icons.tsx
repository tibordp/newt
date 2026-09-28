import React from "react";

// 16px stroke icons for main-window affordances (dialogs, pane and terminal
// chrome), on the same grid as the viewer toolbar set (1.5px stroke, round
// caps, currentColor).

function Icon({ children }: { children: React.ReactNode }) {
  return (
    <svg
      viewBox="0 0 16 16"
      width="16"
      height="16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {children}
    </svg>
  );
}

/// Dual-pane frame with a focused row in the right half.
export function IconRevealInPane() {
  return (
    <Icon>
      <rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.5" />
      <line x1="6.5" y1="2.75" x2="6.5" y2="13.25" />
      <line
        x1="8.5"
        y1="8"
        x2="12.25"
        y2="8"
        strokeWidth="2"
        strokeLinecap="butt"
      />
    </Icon>
  );
}

/// Box with an arrow leaving through its top-right corner.
export function IconOpenExternal() {
  return (
    <Icon>
      <path d="M13.25 9.25v3a1.5 1.5 0 0 1-1.5 1.5h-8a1.5 1.5 0 0 1-1.5-1.5v-8a1.5 1.5 0 0 1 1.5-1.5h3" />
      <polyline points="9.75 2.25 13.75 2.25 13.75 6.25" />
      <line x1="7.25" y1="8.75" x2="13.75" y2="2.25" />
    </Icon>
  );
}

/// Downward chevron for a button that opens a menu.
export function IconChevronDown() {
  return (
    <Icon>
      <polyline points="4.5 6.25 8 9.75 11.5 6.25" />
    </Icon>
  );
}

/// Arrows pointing out of opposite corners.
export function IconMaximize() {
  return (
    <Icon>
      <polyline points="10 2 14 2 14 6" />
      <polyline points="6 14 2 14 2 10" />
      <line x1="14" y1="2" x2="9.5" y2="6.5" />
      <line x1="2" y1="14" x2="6.5" y2="9.5" />
    </Icon>
  );
}

/// Arrows pointing into the center from opposite corners.
export function IconRestore() {
  return (
    <Icon>
      <polyline points="2.5 9.5 6.5 9.5 6.5 13.5" />
      <polyline points="13.5 6.5 9.5 6.5 9.5 2.5" />
      <line x1="9.5" y1="6.5" x2="14" y2="2" />
      <line x1="2" y1="14" x2="6.5" y2="9.5" />
    </Icon>
  );
}

/// Dual-pane frame with one half filled: the pane on screen.
export function IconPaneSide({ side }: { side: number }) {
  return (
    <Icon>
      <rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.5" />
      <line x1="8" y1="2.75" x2="8" y2="13.25" />
      <rect
        x={side === 0 ? 3.5 : 9.75}
        y="4.5"
        width="2.75"
        height="7"
        fill="currentColor"
        stroke="none"
      />
    </Icon>
  );
}

/// A cross: close.
export function IconClose() {
  return (
    <Icon>
      <line x1="4" y1="4" x2="12" y2="12" />
      <line x1="12" y1="4" x2="4" y2="12" />
    </Icon>
  );
}
