import { describe, expect, it } from "vitest";
import { classifyLink, resolveRelative } from "./markdown";

const doc = "/home/me/proj/docs/README.md";

describe("resolveRelative", () => {
  it("resolves against the document's directory", () => {
    expect(resolveRelative(doc, "guide.md")).toBe(
      "/home/me/proj/docs/guide.md",
    );
    expect(resolveRelative(doc, "./img/a.png")).toBe(
      "/home/me/proj/docs/img/a.png",
    );
    expect(resolveRelative(doc, "../LICENSE")).toBe("/home/me/proj/LICENSE");
    expect(resolveRelative(doc, "/etc/hosts")).toBe("/etc/hosts");
    expect(resolveRelative(doc, "../../../../../x")).toBe("/x");
  });

  it("drops the query and fragment and decodes escapes", () => {
    expect(resolveRelative(doc, "My%20Notes.md#intro")).toBe(
      "/home/me/proj/docs/My Notes.md",
    );
    expect(resolveRelative(doc, "100%.md")).toBe("/home/me/proj/docs/100%.md");
    expect(resolveRelative(doc, "#only")).toBeNull();
  });
});

describe("classifyLink", () => {
  it("sends web and mail links out", () => {
    expect(classifyLink("https://example.com/a", doc)).toEqual({
      kind: "web",
      url: "https://example.com/a",
    });
    expect(classifyLink("MAILTO:x@y.z", doc).kind).toBe("web");
  });

  it("ignores every other scheme", () => {
    for (const href of [
      "file:///etc/passwd",
      "javascript:alert(1)",
      "C:/Windows/calc.exe",
      "smb://host/share",
      "//evil.example/x",
    ]) {
      expect(classifyLink(href, doc)).toEqual({ kind: "none" });
    }
  });

  it("keeps anchors in the document and opens relative files", () => {
    expect(classifyLink("#Getting%20Started", doc)).toEqual({
      kind: "anchor",
      id: "Getting Started",
    });
    expect(classifyLink("../src/main.rs", doc)).toEqual({
      kind: "file",
      path: "/home/me/proj/src/main.rs",
    });
    expect(classifyLink(null, doc)).toEqual({ kind: "none" });
  });
});
