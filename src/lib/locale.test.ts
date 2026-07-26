import { describe, it, expect } from "vitest";

import { usableLocale } from "./locale";
import { formatDate } from "./datetime";

describe("usableLocale", () => {
  it("passes through a well-formed tag", () => {
    expect(usableLocale("de-DE")).toBe("de-DE");
    expect(usableLocale("sl")).toBe("sl");
    expect(usableLocale("sr-Latn-RS")).toBe("sr-Latn-RS");
  });

  it("treats absent or empty as no preference", () => {
    expect(usableLocale(undefined)).toBeUndefined();
    expect(usableLocale(null)).toBeUndefined();
    expect(usableLocale("")).toBeUndefined();
  });

  /// The settings dialog writes on every keystroke, so each prefix of the
  /// tag the user is typing reaches the formatting path. `Intl` throws a
  /// RangeError on these rather than degrading.
  it("rejects the partial tags typed on the way to a real one", () => {
    for (const partial of ["s", "sl-", "de-DE-", "-", "!!", "e n"]) {
      expect(usableLocale(partial), partial).toBeUndefined();
    }
  });

  it("passes through a well-formed tag no engine knows", () => {
    // Structurally valid, so Intl degrades to its default rather than
    // throwing — nothing to guard against.
    expect(usableLocale("xx-YY")).toBe("xx-YY");
  });

  it("keeps formatting from throwing on any of them", () => {
    for (const tag of ["s", "sl-", "de-DE", "xx-YY", "", null]) {
      expect(() => formatDate(0, undefined, usableLocale(tag))).not.toThrow();
    }
  });
});
