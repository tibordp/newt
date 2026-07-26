import { describe, it, expect } from "vitest";
import { modeString } from "./utils";

describe("modeString", () => {
  it("regular file 644", () => {
    expect(modeString(0o100644)).toBe("-rw-r--r--");
  });

  it("directory 755", () => {
    expect(modeString(0o040755)).toBe("drwxr-xr-x");
  });

  it("symlink 777", () => {
    expect(modeString(0o120777)).toBe("lrwxrwxrwx");
  });

  it("executable 755", () => {
    expect(modeString(0o100755)).toBe("-rwxr-xr-x");
  });

  it("no permissions", () => {
    expect(modeString(0o100000)).toBe("----------");
  });

  it("setuid with execute", () => {
    expect(modeString(0o104755)).toBe("-rwsr-xr-x");
  });

  it("setuid without execute", () => {
    expect(modeString(0o104644)).toBe("-rwSr--r--");
  });

  it("setgid with execute", () => {
    expect(modeString(0o102755)).toBe("-rwxr-sr-x");
  });

  it("setgid without execute", () => {
    expect(modeString(0o102744)).toBe("-rwxr-Sr--");
  });

  it("sticky with execute", () => {
    expect(modeString(0o101777)).toBe("-rwxrwxrwt");
  });

  it("sticky without execute", () => {
    expect(modeString(0o101776)).toBe("-rwxrwxrwT");
  });

  it("all special bits", () => {
    expect(modeString(0o107777)).toBe("-rwsrwsrwt");
  });

  it("block device", () => {
    expect(modeString(0o060660)).toBe("brw-rw----");
  });

  it("char device", () => {
    expect(modeString(0o020666)).toBe("crw-rw-rw-");
  });

  it("pipe", () => {
    expect(modeString(0o010644)).toBe("prw-r--r--");
  });

  it("socket", () => {
    expect(modeString(0o140755)).toBe("srwxr-xr-x");
  });
});
