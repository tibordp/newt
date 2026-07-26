import { useMemo } from "react";

import { usePreferences } from "./preferences";

/// A locale tag the JS runtime will actually accept, or `undefined` to leave
/// it on its own default.
///
/// `Intl` throws a `RangeError` on a malformed tag rather than degrading, and
/// the settings dialog writes `appearance.locale` on every keystroke — so
/// typing `sl-SI` passes through `s`, `sl`, `sl-`, each of which reaches the
/// file list's date and size formatting. `sl-` alone is enough to take the
/// window down. Validation is delegated to the engine that will consume the
/// value rather than hand-rolled from the BCP-47 grammar, so the two can't
/// disagree.
///
/// Only *well-formedness* is checked: a structurally valid but unknown tag
/// (`xx-YY`) is passed through, and `Intl` falls back to its default for it —
/// no crash, so nothing to guard against.
export function usableLocale(
  tag: string | null | undefined,
): string | undefined {
  if (!tag) return undefined;
  try {
    Intl.getCanonicalLocales(tag);
    return tag;
  } catch {
    return undefined;
  }
}

/// The resolved locale for formatting numbers, dates and times: the
/// `appearance.locale` preference when set and well-formed, else the system's
/// regional format as resolved by the backend. See
/// `ResolvedPreferences.locale`.
export function useLocale(): string | undefined {
  const preferences = usePreferences();
  return useMemo(
    () => usableLocale(preferences?.locale),
    [preferences?.locale],
  );
}
