const SI_PREFIXES_CENTER_INDEX = 10;

const siPrefixes: readonly string[] = [
  "q",
  "r",
  "y",
  "z",
  "a",
  "f",
  "p",
  "n",
  "μ",
  "m",
  "",
  "k",
  "M",
  "G",
  "T",
  "P",
  "E",
  "Z",
  "Y",
  "R",
  "Q",
];

export const getSiPrefixedNumber = (number: number): string => {
  if (number === 0) return number.toString();
  const EXP_STEP_SIZE = 3;
  const base = Math.floor(Math.log10(Math.abs(number)));
  const siBase = (base < 0 ? Math.ceil : Math.floor)(base / EXP_STEP_SIZE);
  const prefix = siPrefixes[siBase + SI_PREFIXES_CENTER_INDEX];

  // return number as-is if no prefix is available
  if (siBase === 0) return number.toString();

  // We're left with a number which needs to be devided by the power of 10e[base]
  // This outcome is then rounded two decimals and parsed as float to make sure those
  // decimals only appear when they're actually requird (10.0 -> 10, 10.90 -> 19.9, 10.01 -> 10.01)
  const baseNumber = parseFloat(
    (number / Math.pow(10, siBase * EXP_STEP_SIZE)).toFixed(2)
  );
  return `${baseNumber} ${prefix}`;
};

export const modeString = (mode: number) => {
  const TYPE_CHARS = "?pc?d?b?-?l?s???";
  const MODE_CHARS = "rwxSTst";

  let ret = Array(10).fill("-");
  let idx = 0;

  ret[idx] = TYPE_CHARS[(mode >> 12) & 0xf];
  let i = 0;
  let m = 0o400;
  while (true) {
    let j = 0;
    let k = 0;

    while (true) {
      idx += 1;
      ret[idx] = "-";
      if ((mode & m) != 0) {
        ret[idx] = MODE_CHARS[j];
        k = j;
      }
      m = m >> 1;
      j += 1;
      if (j >= 3) {
        break;
      }
    }
    i += 1;

    if ((mode & (0o10000 >> i)) != 0) {
      ret[idx] = MODE_CHARS[3 + (k & 2) + ((i == 3) ? 1 : 0)];
    }
    if (i >= 3) {
      break;
    }
  }

  return ret.join("");
};
