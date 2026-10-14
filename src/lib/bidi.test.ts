import { describe, it, expect } from "vitest";

import { visibleName } from "./bidi";

describe("visibleName", () => {
  it("shows bidi formatting characters one for one", () => {
    const spoof = "photo‮gpj.exe";
    expect(visibleName(spoof)).toBe("photo�gpj.exe");
    expect(visibleName(spoof)).toHaveLength(spoof.length);
    expect(visibleName("a⁦b⁩‏c")).toBe("a�b��c");
  });

  it("leaves other text alone, right-to-left included", () => {
    expect(visibleName("report.pdf")).toBe("report.pdf");
    expect(visibleName("שלום.txt")).toBe("שלום.txt");
  });
});
