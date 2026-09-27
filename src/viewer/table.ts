import type { TableDelimiter } from "../lib/bindings";
import type { NewlineScan } from "./helpers";

export const DELIMITER_CHARS: Record<TableDelimiter, string> = {
  comma: ",",
  semicolon: ";",
  tab: "\t",
  pipe: "|",
};

export interface Dialect {
  delimiter: string;
  /// Honour `"` quoting: a quote at the start of a field opens a quoted
  /// field, which may hold delimiters and newlines, with `""` for a quote.
  quoted: boolean;
}

// Parser states. A quote opens a quoted field only at the start of a
// field; a stray quote mid-field is data, and text after a closing quote
// is kept (lenient RFC 4180, like Python's csv module).
const FIELD_START = 0;
const UNQUOTED = 1;
const QUOTED = 2;
const QUOTE_IN_QUOTED = 3;

const LF = 0x0a;
const QUOTE = 0x22;

/// Walks the raw code units of a delimited file and finds where rows
/// start, carrying quote state across chunk boundaries. Row boundaries
/// only depend on newlines and quotes, which no catalogue encoding places
/// inside a multibyte sequence, so a byte scan is exact outside UTF-16.
export class RowScanner {
  private state = FIELD_START;
  private fields = 1;
  private readonly delimiter: number;
  private readonly quote: number;
  private readonly step: 1 | 2;
  /// Most fields in any row finished so far.
  maxFields = 1;

  constructor(
    dialect: Dialect,
    private readonly scan: NewlineScan,
  ) {
    this.delimiter = dialect.delimiter.charCodeAt(0);
    this.quote = dialect.quoted ? QUOTE : -1;
    this.step = scan === "byte" ? 1 : 2;
  }

  /// Index of the field the scan is in (0-based).
  get field(): number {
    return this.fields - 1;
  }

  /// Append the absolute offset of each row start in `chunk` (at absolute
  /// `chunkStart`) from absolute offset `from` on.
  push(chunk: Uint8Array, chunkStart: number, from: number, out: number[]) {
    const step = this.step;
    for (let i = from - chunkStart; i + step <= chunk.length; i += step) {
      if (this.feed(this.unit(chunk, i))) out.push(chunkStart + i + step);
    }
  }

  /// Feed code units until relative offset `end`; used to find the field
  /// a byte offset falls in.
  feedUntil(bytes: Uint8Array, end: number) {
    for (let i = 0; i + this.step <= end; i += this.step) {
      this.feed(this.unit(bytes, i));
    }
  }

  private unit(bytes: Uint8Array, i: number): number {
    if (this.step === 1) return bytes[i];
    return this.scan === "utf16le"
      ? bytes[i] | (bytes[i + 1] << 8)
      : (bytes[i] << 8) | bytes[i + 1];
  }

  /// Advance over one code unit; true when it ends a row.
  private feed(u: number): boolean {
    if (this.state === QUOTED) {
      if (u === this.quote) this.state = QUOTE_IN_QUOTED;
      return false;
    }
    if (this.state === QUOTE_IN_QUOTED && u === this.quote) {
      this.state = QUOTED;
      return false;
    }
    if (u === LF) {
      this.maxFields = Math.max(this.maxFields, this.fields);
      this.fields = 1;
      this.state = FIELD_START;
      return true;
    }
    if (u === this.delimiter) {
      this.fields++;
      this.state = FIELD_START;
      return false;
    }
    this.state =
      this.state === FIELD_START && u === this.quote ? QUOTED : UNQUOTED;
    return false;
  }
}

/// Split decoded text into rows of fields, with the same rules as
/// `RowScanner`. A trailing newline doesn't start another row; `\r\n`
/// ends a row like `\n`.
export function parseRows(text: string, dialect: Dialect): string[][] {
  const rows: string[][] = [];
  let fields: string[] = [];
  let cur = "";
  let state = FIELD_START;
  const { delimiter, quoted } = dialect;
  const endRow = () => {
    if (state !== QUOTED && cur.endsWith("\r")) cur = cur.slice(0, -1);
    fields.push(cur);
    rows.push(fields);
    fields = [];
    cur = "";
    state = FIELD_START;
  };
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (state === QUOTED) {
      if (c === '"') state = QUOTE_IN_QUOTED;
      else cur += c;
      continue;
    }
    if (state === QUOTE_IN_QUOTED && c === '"') {
      cur += '"';
      state = QUOTED;
      continue;
    }
    if (c === "\n") {
      endRow();
    } else if (c === delimiter) {
      fields.push(cur);
      cur = "";
      state = FIELD_START;
    } else if (state === FIELD_START && quoted && c === '"') {
      state = QUOTED;
    } else {
      cur += c;
      state = UNQUOTED;
    }
  }
  if (cur !== "" || fields.length > 0 || state !== FIELD_START) endRow();
  return rows;
}

/// The fields of one row's decoded text.
export function parseRow(text: string, dialect: Dialect): string[] {
  return parseRows(text, dialect)[0] ?? [""];
}

/// The field a byte offset falls in, within the raw bytes of one row.
export function fieldAtOffset(
  rowBytes: Uint8Array,
  offset: number,
  dialect: Dialect,
  scan: NewlineScan,
): number {
  const scanner = new RowScanner(dialect, scan);
  scanner.feedUntil(rowBytes, offset);
  return scanner.field;
}

const NUMERIC = /^\s*[-+]?(\d[\d,]*(\.\d*)?|\.\d+)([eE][-+]?\d+)?\s*%?\s*$/;

export function isNumeric(value: string): boolean {
  return NUMERIC.test(value);
}

/// Complete rows at the start of `sample`: a sample cut mid-file drops
/// its last, possibly partial, row.
function sampleRows(
  sample: string,
  dialect: Dialect,
  complete: boolean,
): string[][] {
  const rows = parseRows(sample, dialect);
  return complete ? rows : rows.slice(0, -1);
}

const CANDIDATES: TableDelimiter[] = ["comma", "semicolon", "tab", "pipe"];

/// The delimiter that splits the sample's rows most consistently into
/// more than one field. `complete` says the sample is the whole file;
/// `hint` (from the extension) breaks ties and is the fallback.
export function detectDelimiter(
  sample: string,
  quoted: boolean,
  complete: boolean,
  hint: TableDelimiter,
): TableDelimiter {
  let best = hint;
  let bestScore = 0;
  let bestFields = 0;
  for (const candidate of CANDIDATES) {
    const rows = sampleRows(
      sample,
      { delimiter: DELIMITER_CHARS[candidate], quoted },
      complete,
    )
      .slice(0, 100)
      .filter((r) => r.length > 1 || r[0] !== "");
    if (rows.length === 0) continue;
    const counts = new Map<number, number>();
    for (const r of rows) counts.set(r.length, (counts.get(r.length) ?? 0) + 1);
    let fields = 0;
    let frequency = 0;
    for (const [n, f] of counts) {
      if (f > frequency || (f === frequency && n > fields)) {
        fields = n;
        frequency = f;
      }
    }
    if (fields < 2) continue;
    const score = frequency / rows.length;
    const better =
      score > bestScore ||
      (score === bestScore &&
        (candidate === hint || (best !== hint && fields > bestFields)));
    if (better) {
      best = candidate;
      bestScore = score;
      bestFields = fields;
    }
  }
  return best;
}

/// Whether the first row looks like a header, by the votes of the columns
/// below it (Python's `csv.Sniffer.has_header`): a column of numbers under
/// a non-number, or of fixed-width values under a label of another width,
/// votes for; the same kind of value in row 1 votes against.
export function detectHeader(
  sample: string,
  dialect: Dialect,
  complete: boolean,
): boolean {
  const rows = sampleRows(sample, dialect, complete).slice(0, 21);
  if (rows.length < 2) return false;
  const [header, ...body] = rows;
  let votes = 0;
  for (let c = 0; c < header.length; c++) {
    const values = body.map((r) => r[c]).filter((v) => v !== undefined && v);
    if (values.length === 0) continue;
    const label = header[c];
    if (values.every(isNumeric)) {
      votes += isNumeric(label) ? -1 : 1;
    } else if (values.every((v) => v.length === values[0].length)) {
      votes += label.length !== values[0].length ? 1 : -1;
    }
  }
  return votes > 0;
}

/// Spreadsheet column name: A–Z, then AA, AB, ….
export function columnName(index: number): string {
  let name = "";
  for (let n = index + 1; n > 0; n = Math.floor((n - 1) / 26)) {
    name = String.fromCharCode(65 + ((n - 1) % 26)) + name;
  }
  return name;
}

/// Rows of fields as delimited text for the clipboard: TSV pastes into
/// spreadsheets as cells. A field holding the separator, a quote or a
/// line break is quoted, `""` for a quote.
export function formatRows(rows: string[][], separator: "\t" | ","): string {
  const special = separator === "\t" ? /[\t\n\r"]/ : /[,\n\r"]/;
  const quote = (f: string) =>
    special.test(f) ? `"${f.replaceAll('"', '""')}"` : f;
  return rows.map((r) => r.map(quote).join(separator)).join("\n");
}
