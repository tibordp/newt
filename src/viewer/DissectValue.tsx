import React from "react";

import type {
  DissectColor,
  DissectFlag,
  DissectValue,
  DissectValueData,
} from "../lib/bindings";
import { formatDateTime } from "../lib/datetime";
import { usableLocale } from "../lib/locale";
import { usePreferences } from "../lib/preferences";
import styles from "./DissectViewer.module.scss";

type Data<K extends DissectValueData["kind"]> = Extract<
  DissectValueData,
  { kind: K }
>;

const VALID = "\u{f012c}";
const MISMATCH = "\u{f0156}";

/// What Copy Value copies: the value itself, not its annotations.
export function copyableValue(value: DissectValue): string {
  switch (value.data.kind) {
    case "text":
      return value.data.value;
    case "checksum":
      return value.data.stored;
    default:
      return value.text;
  }
}

/// The value on its tree line.
export function ValueInline({ value }: { value: DissectValue }) {
  const d = value.data;
  return (
    <>
      {d.kind === "color" && <Swatch color={d} />}
      <span
        className={styles.value}
        data-kind={d.kind}
        data-unknown={d.kind === "enum" && !d.known ? "" : undefined}
      >
        {d.kind === "checksum" ? d.stored : value.text}
      </span>
      {(d.kind === "enum" || d.kind === "flags") && value.raw && (
        <span className={styles.raw}>{value.raw}</span>
      )}
      {d.kind === "checksum" && <CheckInline check={d} />}
    </>
  );
}

function CheckInline({ check }: { check: Data<"checksum"> }) {
  const v = check.verdict;
  return (
    <>
      <span className={styles.note}>{check.algo}</span>
      {v.kind === "valid" && (
        <span
          className={`${styles.verdict} ${styles.verdictMark}`}
          data-verdict="valid"
          title="Matches the data"
        >
          {VALID}
        </span>
      )}
      {v.kind === "mismatch" && (
        <span
          className={`${styles.verdict} ${styles.verdictMark}`}
          data-verdict="mismatch"
          title={
            v.computed
              ? `Doesn't match the data, which gives ${v.computed}`
              : "Doesn't match the data"
          }
        >
          {MISMATCH}
        </span>
      )}
      {(v.kind === "not_checked" || v.kind === "skipped") && (
        <span className={styles.note}>
          {v.kind === "skipped" ? `not checked: ${v.reason}` : "not checked"}
        </span>
      )}
    </>
  );
}

function Swatch({
  color,
  large = false,
}: {
  color: DissectColor;
  large?: boolean;
}) {
  if (!color.swatch) return null;
  return (
    <span
      className={styles.swatch}
      data-large={large ? "" : undefined}
      data-approximate={color.exact ? undefined : ""}
      title={color.exact ? undefined : "Approximate preview"}
      style={{ "--swatch": `#${color.swatch}` } as React.CSSProperties}
    />
  );
}

/// The value's rows in the details list.
export function ValueRows({ value }: { value: DissectValue }) {
  const d = value.data;
  return (
    <>
      <dt>Value</dt>
      <dd className={styles.value} data-kind={d.kind}>
        {d.kind === "color" && <Swatch color={d} large />}
        {value.text}
      </dd>
      <KindRows value={value} />
    </>
  );
}

function Row({
  label,
  mono = false,
  children,
}: {
  label: string;
  mono?: boolean;
  children: React.ReactNode;
}) {
  return (
    <>
      <dt>{label}</dt>
      <dd className={mono ? styles.mono : undefined}>{children}</dd>
    </>
  );
}

function Muted({ children }: { children: React.ReactNode }) {
  return <span className={styles.muted}>{children}</span>;
}

function KindRows({ value }: { value: DissectValue }) {
  const d = value.data;
  switch (d.kind) {
    case "number":
      return <NumberRows n={d} />;
    case "enum":
      return (
        <>
          <Row label="Raw" mono>
            {value.raw} <Muted>({d.raw})</Muted>
          </Row>
          <Row label="Type">{d.bits}-bit enumeration</Row>
        </>
      );
    case "flags":
      return <FlagRows flags={d} />;
    case "float":
      return (
        <Row label="Type">{FLOAT_NAMES[d.bits] ?? `${d.bits}-bit float`}</Row>
      );
    case "time":
      return <TimeRows time={d} />;
    case "text":
      return (
        <Row label="Length">{plural([...d.value].length, "character")}</Row>
      );
    case "bytes":
      return <Row label="Length">{plural(d.len, "byte")}</Row>;
    case "guid":
      return <GuidRows guid={d} text={value.text} />;
    case "checksum":
      return <CheckRows check={d} />;
    case "size":
      return <SizeRows size={d} />;
    case "duration":
      return d.per_sec === "1" ? null : (
        <Row label="Ticks">
          {BigInt(d.ticks).toLocaleString()}{" "}
          <Muted>of 1/{BigInt(d.per_sec).toLocaleString()} s</Muted>
        </Row>
      );
    case "addr":
      return <AddrRows addr={d} />;
    case "color":
      return <ColorRows color={d} />;
    default:
      return value.raw ? (
        <Row label="Raw" mono>
          {value.raw}
        </Row>
      ) : null;
  }
}

function plural(n: number, noun: string) {
  return `${n.toLocaleString()} ${noun}${n === 1 ? "" : "s"}`;
}

/// `n` as the unsigned number of its `bits`.
function unsigned(n: bigint, bits: number) {
  return n < 0n ? n + (1n << BigInt(bits)) : n;
}

function hexDigits(n: bigint, bits: number) {
  return n.toString(16).padStart(Math.ceil(bits / 4), "0");
}

/// Binary digits in groups of four.
function binaryDigits(n: bigint, bits: number) {
  const digits = n.toString(2).padStart(bits, "0");
  return digits.replace(/\B(?=(\d{4})+$)/g, " ");
}

/// The number in the radixes the value doesn't already show it in.
function NumberRows({ n }: { n: Data<"number"> }) {
  const value = BigInt(n.value);
  const bits = unsigned(value, n.bits);
  return (
    <>
      {n.radix !== "dec" && (
        <Row label="Decimal" mono>
          {value.toString()}
        </Row>
      )}
      {n.radix !== "hex" && (
        <Row label="Hex" mono>
          0x{hexDigits(bits, n.bits)}
        </Row>
      )}
      {n.radix !== "bin" && n.bits <= 32 && (
        <Row label="Binary" mono>
          {binaryDigits(bits, n.bits)}
        </Row>
      )}
      <Row label="Type">
        {n.bits}-bit {n.signed ? "signed" : "unsigned"} integer
      </Row>
    </>
  );
}

const FLOAT_NAMES: Record<number, string> = {
  16: "16-bit half-precision float",
  32: "32-bit single-precision float",
  64: "64-bit double-precision float",
  80: "80-bit extended-precision float",
};

function trailingZeros(n: bigint) {
  let shift = 0n;
  while (n > 0n && (n & 1n) === 0n) {
    n >>= 1n;
    shift++;
  }
  return shift;
}

function FlagRows({ flags }: { flags: Data<"flags"> }) {
  const raw = BigInt(flags.raw);
  const unknown = BigInt(flags.unknown);
  return (
    <>
      <Row label="Raw" mono>
        0x{hexDigits(raw, flags.bits)}
      </Row>
      {flags.table.length > 0 && (
        <>
          <dt>Flags</dt>
          <dd>
            <ul className={styles.flagTable}>
              {flags.table.map((flag, i) => (
                <FlagLine key={i} flag={flag} raw={raw} bits={flags.bits} />
              ))}
            </ul>
          </dd>
        </>
      )}
      {unknown !== 0n && (
        <Row label="Unnamed bits" mono>
          0x{hexDigits(unknown, flags.bits)}
        </Row>
      )}
      <Row label="Type">{flags.bits}-bit flags</Row>
    </>
  );
}

function FlagLine({
  flag,
  raw,
  bits,
}: {
  flag: DissectFlag;
  raw: bigint;
  bits: number;
}) {
  const mask = BigInt(flag.mask);
  const value = BigInt(flag.value);
  const field = (raw & mask) >> trailingZeros(mask);
  return (
    <li data-set={flag.set || flag.number ? "" : undefined}>
      <span className={styles.mono}>
        0x{hexDigits(mask, bits)}
        {!flag.number && mask !== value && ` = 0x${value.toString(16)}`}
      </span>
      <span>{flag.name}</span>
      {flag.number && <span className={styles.mono}>{field.toString()}</span>}
    </li>
  );
}

const RES_UNITS = ["s", "ms", "µs", "ns"];

function resolution(res: Data<"time">["res"]) {
  switch (res.kind) {
    case "day":
      return "1 day";
    case "minute":
      return "1 minute";
    case "second":
      return "1 second";
    case "sub": {
      // 10^-digits of a second, in the largest unit that keeps it whole.
      const unit = Math.ceil(res.digits / 3);
      return `${10 ** (unit * 3 - res.digits)} ${RES_UNITS[unit]}`;
    }
    default:
      return null;
  }
}

function offsetText(minutes: number) {
  const sign = minutes < 0 ? "−" : "+";
  const m = Math.abs(minutes);
  return `UTC${sign}${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
}

function TimeRows({ time }: { time: Data<"time"> }) {
  const preferences = usePreferences();
  const appearance = preferences?.settings?.appearance;
  const ms = Number(time.secs) * 1000 + Math.floor(time.nanos / 1e6);
  const instant =
    (time.zone.kind === "utc" || time.zone.kind === "offset") &&
    time.res.kind !== "day" &&
    Number.isFinite(new Date(ms).getTime());
  const res = resolution(time.res);
  const fraction =
    time.res.kind === "sub" && time.nanos > 0
      ? `.${String(time.nanos).padStart(9, "0").slice(0, time.res.digits)}`
      : "";
  return (
    <>
      {instant && (
        <Row label="Local time">
          {formatDateTime(
            ms,
            appearance?.date_format,
            appearance?.time_format,
            usableLocale(preferences?.locale),
          )}
        </Row>
      )}
      <Row label="Zone">
        {time.zone.kind === "utc" && "UTC"}
        {time.zone.kind === "offset" && (
          <>
            {offsetText(time.zone.minutes)} <Muted>(as recorded)</Muted>
          </>
        )}
        {time.zone.kind === "local" && (
          <>
            Local <Muted>(the file doesn't record which zone)</Muted>
          </>
        )}
        {time.zone.kind === "other" && <Muted>Unknown</Muted>}
      </Row>
      {res && <Row label="Resolution">{res}</Row>}
      <Row label="Unix time" mono>
        {time.secs}
        {fraction}
        {time.zone.kind === "local" && <Muted> (as if UTC)</Muted>}
      </Row>
    </>
  );
}

const UUID_VERSIONS: Record<number, string> = {
  1: "1 (time and node)",
  2: "2 (DCE security)",
  3: "3 (name, MD5)",
  4: "4 (random)",
  5: "5 (name, SHA-1)",
  6: "6 (reordered time)",
  7: "7 (Unix time)",
  8: "8 (custom)",
};

/// 100 ns intervals from the Gregorian reform (1582-10-15) to 1970.
const GREGORIAN_OFFSET = 122192928000000000n;

/// When a time-based UUID was made, in Unix milliseconds.
function uuidTime(hex: string, version: number): number | null {
  switch (version) {
    case 1:
    case 6: {
      const ticks =
        version === 1
          ? BigInt(
              `0x${hex.slice(13, 16)}${hex.slice(8, 12)}${hex.slice(0, 8)}`,
            )
          : BigInt(`0x${hex.slice(0, 12)}${hex.slice(13, 16)}`);
      return Number((ticks - GREGORIAN_OFFSET) / 10000n);
    }
    case 7:
      return parseInt(hex.slice(0, 12), 16);
    default:
      return null;
  }
}

function GuidRows({ guid, text }: { guid: Data<"guid">; text: string }) {
  const hex = guid.uuid.replace(/-/g, "");
  const variant = parseInt(hex[16], 16);
  const version = parseInt(hex[12], 16);
  const rfc = variant >= 8 && variant <= 11;
  const time = rfc ? uuidTime(hex, version) : null;
  return (
    <>
      {guid.uuid !== text.toLowerCase() && (
        <Row label="UUID" mono>
          {guid.uuid}
        </Row>
      )}
      <Row label="Byte order">
        {guid.order === "mixed" &&
          "Microsoft (first three fields little-endian)"}
        {guid.order === "big" && "RFC 4122 (big-endian)"}
        {guid.order === "other" && <Muted>Unknown</Muted>}
      </Row>
      {/^0+$/.test(hex) ? (
        <Row label="Version">Nil</Row>
      ) : /^f+$/.test(hex) ? (
        <Row label="Version">Max</Row>
      ) : rfc ? (
        <Row label="Version">{UUID_VERSIONS[version] ?? version}</Row>
      ) : (
        <Row label="Variant">
          {variant < 8
            ? "NCS (pre-RFC 4122)"
            : variant < 14
              ? "Microsoft"
              : "Reserved"}
        </Row>
      )}
      {time !== null && Number.isFinite(new Date(time).getTime()) && (
        <Row label="Created">
          {new Date(time).toISOString().replace("T", " ").replace("Z", " UTC")}
        </Row>
      )}
    </>
  );
}

function CheckRows({ check }: { check: Data<"checksum"> }) {
  const v = check.verdict;
  return (
    <>
      <Row label="Algorithm">{check.algo}</Row>
      <Row label="Stored" mono>
        {check.stored}
      </Row>
      <Row label="Verdict">
        {v.kind === "valid" && (
          <span className={styles.verdict} data-verdict="valid">
            Matches the data
          </span>
        )}
        {v.kind === "mismatch" && (
          <span className={styles.verdict} data-verdict="mismatch">
            Doesn't match the data
          </span>
        )}
        {v.kind === "not_checked" && <Muted>Not checked</Muted>}
        {v.kind === "skipped" && <Muted>Not checked: {v.reason}</Muted>}
        {v.kind === "deferred" && (
          <Muted>Checked as the content is decoded</Muted>
        )}
        {v.kind === "other" && <Muted>Unknown</Muted>}
      </Row>
      {v.kind === "mismatch" && v.computed && (
        <Row label="Computed" mono>
          {v.computed}
        </Row>
      )}
    </>
  );
}

function SizeRows({ size }: { size: Data<"size"> }) {
  return (
    <>
      {size.bytes !== null && (
        <Row label="Bytes">{BigInt(size.bytes).toLocaleString()}</Row>
      )}
      {size.unit !== null && size.unit > 1 && (
        <Row label="Blocks">
          {BigInt(size.n).toLocaleString()}{" "}
          <Muted>of {size.unit.toLocaleString()} bytes</Muted>
        </Row>
      )}
      <Row label="Type">
        {size.bits}-bit count of {size.unit === 1 ? "bytes" : "blocks"}
      </Row>
    </>
  );
}

function addrScope(addr: Data<"addr">): string | null {
  const b = addr.hex.match(/../g)?.map((x) => parseInt(x, 16)) ?? [];
  switch (addr.family) {
    case "ipv4": {
      const [a, c] = b;
      if (b.every((x) => x === 0)) return "Unspecified";
      if (b.every((x) => x === 255)) return "Broadcast";
      if (a === 127) return "Loopback";
      if (
        a === 10 ||
        (a === 172 && (c & 0xf0) === 16) ||
        (a === 192 && c === 168)
      )
        return "Private";
      if (a === 169 && c === 254) return "Link-local";
      if (a === 100 && (c & 0xc0) === 64) return "Shared (carrier-grade NAT)";
      if ((a & 0xf0) === 224) return "Multicast";
      return "Public";
    }
    case "ipv6": {
      if (b.every((x) => x === 0)) return "Unspecified";
      if (b.slice(0, 15).every((x) => x === 0) && b[15] === 1)
        return "Loopback";
      if (
        b.slice(0, 10).every((x) => x === 0) &&
        b[10] === 255 &&
        b[11] === 255
      )
        return "IPv4-mapped";
      if (b[0] === 0xff) return "Multicast";
      if (b[0] === 0xfe && (b[1] & 0xc0) === 0x80) return "Link-local";
      if ((b[0] & 0xfe) === 0xfc) return "Unique local";
      return "Global";
    }
    case "mac":
    case "eui64":
      return `${b[0] & 1 ? "Multicast" : "Unicast"}, ${
        b[0] & 2 ? "locally administered" : "globally unique"
      }`;
    default:
      return null;
  }
}

const ADDR_FAMILIES: Record<Data<"addr">["family"], string> = {
  ipv4: "IPv4",
  ipv6: "IPv6",
  mac: "MAC (EUI-48)",
  eui64: "EUI-64",
  other: "Unknown",
};

function AddrRows({ addr }: { addr: Data<"addr"> }) {
  const scope = addrScope(addr);
  return (
    <>
      <Row label="Family">{ADDR_FAMILIES[addr.family]}</Row>
      {scope && <Row label="Scope">{scope}</Row>}
    </>
  );
}

const COLOR_MODELS: Record<DissectColor["model"], [string, string[]]> = {
  rgb: ["RGB", ["R", "G", "B"]],
  gray: ["Grayscale", ["Gray"]],
  cmyk: ["CMYK", ["C", "M", "Y", "K"]],
  lab: ["CIE L*a*b*", ["L*", "a*", "b*"]],
  hsb: ["HSB", ["H", "S", "B"]],
  other: ["Unknown", []],
};

const COLOR_SPACES: Record<DissectColor["space"], string> = {
  srgb: "sRGB",
  "srgb-linear": "Linear sRGB",
  "display-p3": "Display P3",
  rec2020: "Rec. 2020",
  device: "Device (no stated space)",
  other: "Unknown",
};

/// A component in the terms the file stores it in: the integer of its
/// depth, or the fraction; Lab in its own units, hue in degrees.
function component(color: DissectColor, i: number): string {
  const c = color.components[i];
  if (color.model === "lab") return c.toFixed(1);
  if (color.model === "hsb" && i === 0) return `${Math.round(c * 360)}°`;
  const depth = color.depth;
  const bits =
    depth.kind === "bits" ? depth.bits : depth.kind === "packed" ? 8 : null;
  if (bits !== null) return String(Math.round(c * (2 ** bits - 1)));
  return String(Number(c.toFixed(4)));
}

function ColorRows({ color }: { color: DissectColor }) {
  const [model, labels] = COLOR_MODELS[color.model];
  const depth = color.depth;
  return (
    <>
      {color.name && <Row label="Name">{color.name}</Row>}
      <Row label="Model">{model}</Row>
      {labels.length > 0 && (
        <Row label="Components" mono>
          {labels.map((label, i) => (
            <span key={label} className={styles.component}>
              <Muted>{label}</Muted> {component(color, i)}
            </span>
          ))}
        </Row>
      )}
      {color.alpha && (
        <Row label="Alpha">
          {Number((color.alpha.value * 100).toFixed(1))}%
          {color.alpha.premultiplied && <Muted> (premultiplied)</Muted>}
        </Row>
      )}
      <Row label="Depth">
        {depth.kind === "bits" && `${depth.bits} bits per component`}
        {depth.kind === "float" && `${depth.bits}-bit float components`}
        {depth.kind === "packed" && (
          <>
            {depth.layout} in {depth.bits} bits{" "}
            <span className={styles.mono}>{depth.raw}</span>
          </>
        )}
        {depth.kind === "other" && <Muted>Unknown</Muted>}
      </Row>
      <Row label="Space">{COLOR_SPACES[color.space]}</Row>
      {color.swatch && (
        <Row label="Shown as" mono>
          #
          {color.swatch.endsWith("ff")
            ? color.swatch.slice(0, 6)
            : color.swatch}
          {!color.exact && <Muted> (approximately, in sRGB)</Muted>}
        </Row>
      )}
    </>
  );
}
