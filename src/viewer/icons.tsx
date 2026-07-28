import React from "react";

// 16px stroke icons for the viewer toolbar, drawn on a shared grid so they
// read as one set (1.5px stroke, round caps, currentColor).

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

export function IconMinus() {
  return (
    <Icon>
      <line x1="3.5" y1="8" x2="12.5" y2="8" />
    </Icon>
  );
}

export function IconPlus() {
  return (
    <Icon>
      <line x1="3.5" y1="8" x2="12.5" y2="8" />
      <line x1="8" y1="3.5" x2="8" y2="12.5" />
    </Icon>
  );
}

export function IconRotateCw() {
  return (
    <Icon>
      <path d="M14 8a6 6 0 1 1-6-6c1.7 0 3.3.7 4.5 1.8L14 5.3" />
      <polyline points="14 2 14 5.3 10.7 5.3" />
    </Icon>
  );
}

export function IconRotateCcw() {
  return (
    <Icon>
      <path d="M2 8a6 6 0 1 0 6-6C6.3 2 4.7 2.7 3.5 3.8L2 5.3" />
      <polyline points="2 2 2 5.3 5.3 5.3" />
    </Icon>
  );
}

export function IconFlipH() {
  return (
    <Icon>
      <line x1="8" y1="2" x2="8" y2="14" strokeDasharray="2 2.5" />
      <polyline points="5.5 5 2.5 8 5.5 11" />
      <polyline points="10.5 5 13.5 8 10.5 11" />
    </Icon>
  );
}

export function IconFlipV() {
  return (
    <Icon>
      <line x1="2" y1="8" x2="14" y2="8" strokeDasharray="2 2.5" />
      <polyline points="5 5.5 8 2.5 11 5.5" />
      <polyline points="5 10.5 8 13.5 11 10.5" />
    </Icon>
  );
}

export function IconChecker() {
  return (
    <Icon>
      <rect x="2.5" y="2.5" width="11" height="11" rx="1" />
      <rect
        x="2.5"
        y="2.5"
        width="5.5"
        height="5.5"
        fill="currentColor"
        stroke="none"
        opacity="0.45"
      />
      <rect
        x="8"
        y="8"
        width="5.5"
        height="5.5"
        fill="currentColor"
        stroke="none"
        opacity="0.45"
      />
    </Icon>
  );
}

export function IconInfo() {
  return (
    <Icon>
      <circle cx="8" cy="8" r="6" />
      <line x1="8" y1="7.4" x2="8" y2="11" />
      <circle cx="8" cy="4.9" r="0.5" fill="currentColor" strokeWidth="0.8" />
    </Icon>
  );
}
