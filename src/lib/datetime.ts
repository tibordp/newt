/// Minimal strftime for the `appearance.date_format` / `time_format`
/// preferences. Unrecognized specifiers are rendered literally.

const pad = (n: number, width = 2, fill = "0") =>
  String(n).padStart(width, fill);

function dayOfYear(d: Date): number {
  const start = new Date(d.getFullYear(), 0, 1);
  return Math.floor((d.getTime() - start.getTime()) / 86400000) + 1;
}

/// Month and weekday names for %b/%B/%a/%A. These follow the locale too:
/// setting `date_format` picks the *layout*, not the language.
const localeName = (
  d: Date,
  options: Intl.DateTimeFormatOptions,
  locale?: string,
): string => d.toLocaleDateString(locale, options);

export function strftime(d: Date, fmt: string, locale?: string): string {
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
        return localeName(d, { month: "short" }, locale);
      case "B":
        return localeName(d, { month: "long" }, locale);
      case "a":
        return localeName(d, { weekday: "short" }, locale);
      case "A":
        return localeName(d, { weekday: "long" }, locale);
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

/// Empty/undefined format falls back to locale rendering. `locale` is the
/// resolved `ResolvedPreferences.locale` — pass it rather than relying on the
/// runtime default, which on Windows comes from the display language instead
/// of the regional format.
export function formatDate(ms: number, fmt?: string, locale?: string): string {
  const d = new Date(ms);
  return fmt ? strftime(d, fmt, locale) : d.toLocaleDateString(locale);
}

export function formatTime(ms: number, fmt?: string, locale?: string): string {
  const d = new Date(ms);
  return fmt ? strftime(d, fmt, locale) : d.toLocaleTimeString(locale);
}

export function formatDateTime(
  ms: number,
  dateFmt?: string,
  timeFmt?: string,
  locale?: string,
): string {
  if (!dateFmt && !timeFmt) return new Date(ms).toLocaleString(locale);
  return `${formatDate(ms, dateFmt, locale)} ${formatTime(ms, timeFmt, locale)}`;
}
