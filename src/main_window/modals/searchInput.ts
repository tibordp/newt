import type { TimeBound } from "../../lib/bindings";

/// A dialog field read into what the search takes: `null` when empty.
export type Parsed<T> = { value: T | null } | { error: string };

const SIZE_PREFIXES = ["", "k", "m", "g", "t"];

/// `10 MB`, `1.5G`, `512 KiB`, `300`: decimal prefixes unless an `i`
/// makes them binary, bytes without one.
export function parseSize(text: string): Parsed<number> {
  const trimmed = text.trim();
  if (!trimmed) return { value: null };
  const m = /^(\d+(?:\.\d+)?)\s*([kmgt]?)(i?)b?$/i.exec(trimmed);
  if (!m || (m[3] && !m[2])) {
    return { error: `“${trimmed}” isn't a size (e.g. 10 MB, 512 KiB)` };
  }
  const power = SIZE_PREFIXES.indexOf(m[2].toLowerCase());
  const base = m[3] ? 1024 : 1000;
  return { value: Math.round(parseFloat(m[1]) * base ** power) };
}

const SIZE_UNITS: [number, string][] = [
  [1e12, "TB"],
  [2 ** 40, "TiB"],
  [1e9, "GB"],
  [2 ** 30, "GiB"],
  [1e6, "MB"],
  [2 ** 20, "MiB"],
  [1e3, "kB"],
  [2 ** 10, "KiB"],
];

/// In the largest unit that's exact, so it reads back unchanged.
export function formatSize(bytes: number): string {
  const unit = SIZE_UNITS.find(([n]) => bytes >= n && bytes % n === 0);
  return unit ? `${bytes / unit[0]} ${unit[1]}` : `${bytes} B`;
}

const DAY = 86_400;

/// Duration units by every spelling accepted; the first is the one
/// written back. `m` alone is left out: minutes or months?
const DURATION_UNITS: [number, string[]][] = [
  [365 * DAY, ["y", "year", "years"]],
  [30 * DAY, ["mo", "month", "months"]],
  [7 * DAY, ["w", "week", "weeks"]],
  [DAY, ["d", "day", "days"]],
  [3_600, ["h", "hour", "hours"]],
  [60, ["min", "minute", "minutes"]],
  [1, ["s", "sec", "second", "seconds"]],
];

/// `7d`, `2 weeks`, `3h`: that long before the search starts. `2026-01-31`:
/// the start of that day where the user is.
export function parseTimeBound(text: string): Parsed<TimeBound> {
  const trimmed = text.trim();
  if (!trimmed) return { value: null };
  const duration = /^(\d+)\s*([a-z]+)$/i.exec(trimmed);
  if (duration) {
    const unit = DURATION_UNITS.find(([, names]) =>
      names.includes(duration[2].toLowerCase()),
    );
    if (unit) {
      return { value: { ago: { seconds: Number(duration[1]) * unit[0] } } };
    }
  }
  const date = /^(\d{4})-(\d{2})-(\d{2})$/.exec(trimmed);
  if (date) {
    const [y, m, d] = [Number(date[1]), Number(date[2]), Number(date[3])];
    const start = new Date(y, m - 1, d);
    if (
      start.getFullYear() === y &&
      start.getMonth() === m - 1 &&
      start.getDate() === d
    ) {
      return { value: { date: { date: trimmed, at: start.getTime() } } };
    }
  }
  return {
    error: `“${trimmed}” is neither a time span (7d, 2w, 3h, 6mo) nor a date (2026-01-31)`,
  };
}

export function formatTimeBound(bound: TimeBound): string {
  if ("date" in bound) return bound.date.date;
  const { seconds } = bound.ago;
  const [n, [name]] = DURATION_UNITS.find(
    ([n]) => seconds >= n && seconds % n === 0,
  ) ?? [1, ["s"]];
  return `${seconds / n}${name}`;
}

/// Octal permission bits: `755`, `4000`.
export function parseMode(text: string): Parsed<number> {
  const trimmed = text.trim();
  if (!trimmed) return { value: null };
  if (!/^[0-7]{1,4}$/.test(trimmed)) {
    return { error: `“${trimmed}” isn't octal permission bits (e.g. 755)` };
  }
  return { value: parseInt(trimmed, 8) };
}

export function parseCount(text: string, what: string): Parsed<number> {
  const trimmed = text.trim();
  if (!trimmed) return { value: null };
  if (!/^\d+$/.test(trimmed) || Number(trimmed) === 0) {
    return { error: `${what} must be a whole number above zero` };
  }
  return { value: Number(trimmed) };
}
