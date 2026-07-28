import React from "react";

// 16px stroke icons for dialog affordances, on the same grid as the viewer
// toolbar set (1.5px stroke, round caps, currentColor).

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
