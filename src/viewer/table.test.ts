import { describe, expect, it } from "vitest";
import {
  RowScanner,
  columnName,
  detectDelimiter,
  detectHeader,
  fieldAtOffset,
  formatRows,
  parseRows,
  type Dialect,
} from "./table";

const csv: Dialect = { delimiter: ",", quoted: true };
const bytes = (s: string) => new TextEncoder().encode(s);

function rowStarts(
  text: string,
  dialect: Dialect,
  chunkSize: number,
  scan: "byte" | "utf16le" = "byte",
) {
  const data =
    scan === "byte"
      ? bytes(text)
      : new Uint8Array(
          new Uint16Array([...text].map((c) => c.charCodeAt(0))).buffer,
        );
  const scanner = new RowScanner(dialect, scan);
  const out = [0];
  for (let start = 0; start < data.length; start += chunkSize) {
    scanner.push(data.subarray(start, start + chunkSize), start, start, out);
  }
  return { out, maxFields: scanner.maxFields };
}

describe("RowScanner", () => {
  const text = 'a,b,c\n"x,\ny",2,3\n"q""\n",5,"6"\r\nlast,1';

  it("ends rows at newlines outside quotes, across any chunking", () => {
    const expected = [0, 6, 17, 31];
    for (const size of [1, 2, 3, 5, 7, 64]) {
      expect(rowStarts(text, csv, size).out).toEqual(expected);
    }
    expect(rowStarts(text, csv, 4).maxFields).toBe(3);
  });

  it("treats quotes as data when quoting is off", () => {
    const { out } = rowStarts(
      'a\t"b\nc\td"\n',
      { delimiter: "\t", quoted: false },
      3,
    );
    expect(out).toEqual([0, 5, 10]);
  });

  it("only opens a quote at the start of a field", () => {
    expect(rowStarts('ab"c\nd\n', csv, 2).out).toEqual([0, 5, 7]);
  });

  it("scans UTF-16 by code unit", () => {
    const { out } = rowStarts('a,"b\nc"\nd\n', csv, 4, "utf16le");
    expect(out).toEqual([0, 16, 20]);
  });
});

describe("parseRows", () => {
  it("unquotes fields and splits rows", () => {
    expect(
      parseRows('a,b,c\n"x,\ny",2,3\n"q""\n",5,"6"\r\nlast,1', csv),
    ).toEqual([
      ["a", "b", "c"],
      ["x,\ny", "2", "3"],
      ['q"\n', "5", "6"],
      ["last", "1"],
    ]);
  });

  it("keeps empty fields and rows", () => {
    expect(parseRows(",a,\n\nb\n", csv)).toEqual([["", "a", ""], [""], ["b"]]);
  });

  it("is lenient about stray quotes", () => {
    expect(parseRows('ab"c,"x"y\n', csv)).toEqual([['ab"c', "xy"]]);
  });
});

describe("fieldAtOffset", () => {
  it("finds the field a byte falls in, quotes included", () => {
    const row = bytes('a,"b,c",d\n');
    expect(fieldAtOffset(row, 0, csv, "byte")).toBe(0);
    expect(fieldAtOffset(row, 4, csv, "byte")).toBe(1);
    expect(fieldAtOffset(row, 8, csv, "byte")).toBe(2);
  });
});

describe("detection", () => {
  it("picks the delimiter that splits rows consistently", () => {
    const semicolons = "name;price\nApple, red;1,5\nPear;2\n";
    expect(detectDelimiter(semicolons, true, true, "comma")).toBe("semicolon");
    const tabs = "a\tb\tc\n1\t2\t3\n";
    expect(detectDelimiter(tabs, true, true, "comma")).toBe("tab");
    expect(detectDelimiter("just text\n", true, true, "tab")).toBe("tab");
  });

  it("drops the partial last row of a cut sample", () => {
    expect(detectDelimiter("a,b\n1,2\n3;4;5;6;7", true, false, "comma")).toBe(
      "comma",
    );
  });

  it("recognizes a header over numbers", () => {
    expect(detectHeader("id,price\n1,2.5\n2,3\n", csv, true)).toBe(true);
    expect(detectHeader("1,2.5\n2,3\n3,4\n", csv, true)).toBe(false);
    expect(detectHeader("only\n", csv, true)).toBe(false);
  });
});

describe("formatting", () => {
  it("names columns like a spreadsheet", () => {
    expect([0, 25, 26, 51, 52, 701, 702].map(columnName)).toEqual([
      "A",
      "Z",
      "AA",
      "AZ",
      "BA",
      "ZZ",
      "AAA",
    ]);
  });

  it("quotes only what needs it", () => {
    const rows = [
      ["a", 'b"c', "d\te"],
      ["x,y", "1\n2", ""],
    ];
    expect(formatRows(rows, "\t")).toBe('a\t"b""c"\t"d\te"\nx,y\t"1\n2"\t');
    expect(formatRows(rows, ",")).toBe('a,"b""c",d\te\n"x,y","1\n2",');
  });
});
