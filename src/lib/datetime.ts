/// Minimal strftime for the `appearance.date_format` / `time_format`
/// preferences. Unrecognized specifiers are rendered literally.

const pad = (n: number, width = 2, fill = "0") =>
  String(n).padStart(width, fill);

function dayOfYear(d: Date): number {
  const start = new Date(d.getFullYear(), 0, 1);
  return Math.floor((d.getTime() - start.getTime()) / 86400000) + 1;
}

const localeName = (d: Date, options: Intl.DateTimeFormatOptions): string =>
  d.toLocaleDateString(undefined, options);

export function strftime(d: Date, fmt: string): string {
  return fmt.replace(/%(.)/g, (match, c: string) => {
    switch (c) {
      case "Y":
        return String(d.getFullYear());
      case "y":
        return pad(d.getFullYear() % 100);
      case "m":
        return pad(d.getMonth() + 1);
      case "d":
        return pad(d.getDate());
      case "e":
        return pad(d.getDate(), 2, " ");
      case "j":
        return pad(dayOfYear(d), 3);
      case "b":
        return localeName(d, { month: "short" });
      case "B":
        return localeName(d, { month: "long" });
      case "a":
        return localeName(d, { weekday: "short" });
      case "A":
        return localeName(d, { weekday: "long" });
      case "H":
        return pad(d.getHours());
      case "I":
        return pad(d.getHours() % 12 || 12);
      case "M":
        return pad(d.getMinutes());
      case "S":
        return pad(d.getSeconds());
      case "p":
        return d.getHours() < 12 ? "AM" : "PM";
      case "%":
        return "%";
      default:
        return match;
    }
  });
}

/// Empty/undefined format falls back to the system locale rendering.
export function formatDate(ms: number, fmt?: string): string {
  const d = new Date(ms);
  return fmt ? strftime(d, fmt) : d.toLocaleDateString();
}

export function formatTime(ms: number, fmt?: string): string {
  const d = new Date(ms);
  return fmt ? strftime(d, fmt) : d.toLocaleTimeString();
}

export function formatDateTime(
  ms: number,
  dateFmt?: string,
  timeFmt?: string,
): string {
  if (!dateFmt && !timeFmt) return new Date(ms).toLocaleString();
  return `${formatDate(ms, dateFmt)} ${formatTime(ms, timeFmt)}`;
}
