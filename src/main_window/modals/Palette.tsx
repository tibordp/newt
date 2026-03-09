import {
  useState,
  useCallback,
  useRef,
  ReactNode,
  ReactElement,
  KeyboardEvent,
} from "react";
import { Command } from "cmdk";

/**
 * Shared wrapper around cmdk's Command that adds:
 * - Loop (wrap-around) navigation
 * - PageUp / PageDown (±10 items)
 * - Home / End (first / last item)
 * - Scroll-into-view on all navigation
 */
export function Palette({
  onKeyDown: externalOnKeyDown,
  children,
  ...props
}: {
  onKeyDown?: (e: KeyboardEvent<HTMLDivElement>) => void;
  children: ReactNode;
} & Omit<
  React.ComponentProps<typeof Command>,
  "loop" | "value" | "onValueChange" | "onKeyDown"
>) {
  const [value, setValue] = useState("");
  const containerRef = useRef<HTMLDivElement>(null);

  const handleValueChange = useCallback((v: string) => {
    setValue(v);
    requestAnimationFrame(() => {
      const el = containerRef.current?.querySelector(
        `[cmdk-item][data-value="${CSS.escape(v)}"]`,
      );
      el?.scrollIntoView({ block: "nearest" });
    });
  }, []);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      externalOnKeyDown?.(e);
      if (e.defaultPrevented) return;

      const items = e.currentTarget.querySelectorAll("[cmdk-item]");
      if (!items.length) return;

      const values = Array.from(items).map(
        (el) => el.getAttribute("data-value") ?? "",
      );
      const currentIdx = values.indexOf(value);

      let next: number | null = null;
      if (e.key === "PageDown") {
        next = Math.min(
          (currentIdx === -1 ? 0 : currentIdx) + 10,
          values.length - 1,
        );
      } else if (e.key === "PageUp") {
        next = Math.max((currentIdx === -1 ? 0 : currentIdx) - 10, 0);
      } else if (e.key === "Home") {
        next = 0;
      } else if (e.key === "End") {
        next = values.length - 1;
      }

      if (next !== null) {
        e.preventDefault();
        handleValueChange(values[next]);
      }
    },
    [value, externalOnKeyDown, handleValueChange],
  );

  return (
    <Command
      ref={containerRef}
      loop
      value={value}
      onValueChange={handleValueChange}
      onKeyDown={handleKeyDown}
      {...props}
    >
      {children}
    </Command>
  );
}

export function fuzzyMatch(
  filter: string,
  text: string,
): { matches: boolean; score: number } {
  let a = 0;
  let b = 0;
  let consecutive = 0;
  let maxConsecutive = 0;

  while (a < filter.length && b < text.length) {
    if (filter[a].toLowerCase() === text[b].toLowerCase()) {
      consecutive++;
      a++;
      b++;
    } else {
      maxConsecutive = Math.max(maxConsecutive, consecutive);
      consecutive = 0;
      b++;
    }
  }

  return {
    matches: a === filter.length,
    score: Math.max(maxConsecutive, consecutive),
  };
}

export function Highlight({
  text,
  filter,
  highlightClass,
}: {
  text: string;
  filter: string;
  highlightClass: string;
}) {
  let a = 0;
  let b = 0;
  let key = 0;
  const parts: ReactElement[] = [];

  while (a < filter.length && b < text.length) {
    if (filter[a].toLowerCase() === text[b].toLowerCase()) {
      parts.push(
        <span key={key++} className={highlightClass}>
          {text[b]}
        </span>,
      );
      a++;
      b++;
    } else {
      parts.push(<span key={key++}>{text[b]}</span>);
      b++;
    }
  }

  if (b < text.length) {
    parts.push(<span key={key}>{text.slice(b)}</span>);
  }

  return <span>{parts}</span>;
}
