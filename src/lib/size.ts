import { useMemo } from "react";

import type { SizeUnits } from "./bindings";
import { usableLocale } from "./locale";
import { usePreferences } from "./preferences";

const SCALES: Record<SizeUnits, { base: number; labels: readonly string[] }> = {
  decimal: { base: 1000, labels: ["B", "kB", "MB", "GB", "TB", "PB", "EB"] },
  binary: {
    base: 1024,
    labels: ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"],
  },
};

/// A byte count as a prefixed size: `1.5 GB` decimal, `1.4 GiB` binary.
/// Base and prefixes are defined as a pair, so the two cannot drift apart.
///
/// Up to two decimals, trailing zeros dropped, rendered in `locale` so the
/// separator matches the exact byte counts shown beside it.
export function formatBytes(
  bytes: number,
  units: SizeUnits = "decimal",
  locale?: string,
): string {
  const { base, labels } = SCALES[units] ?? SCALES.decimal;
  let value = bytes;
  let scale = 0;
  while (value >= base && scale < labels.length - 1) {
    value /= base;
    scale += 1;
  }
  const rounded = parseFloat(value.toFixed(2));
  const text = rounded.toLocaleString(locale, { maximumFractionDigits: 2 });
  return `${text} ${labels[scale]}`;
}

/// Size formatter bound to the current preferences.
export function useFormatBytes(): (bytes: number) => string {
  const preferences = usePreferences();
  const units = preferences?.settings?.appearance?.size_units ?? "decimal";
  const locale = usableLocale(preferences?.locale);
  return useMemo(
    () => (bytes: number) => formatBytes(bytes, units, locale),
    [units, locale],
  );
}

/// For call sites that pass the units down rather than formatting in place.
export function useSizeUnits(): SizeUnits {
  const preferences = usePreferences();
  return preferences?.settings?.appearance?.size_units ?? "decimal";
}
