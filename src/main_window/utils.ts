export const modeString = (mode: number) => {
  const TYPE_CHARS = "?pc?d?b?-?l?s???";
  const MODE_CHARS = "rwxSTst";

  const ret = Array(10).fill("-");
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
      ret[idx] = MODE_CHARS[3 + (k & 2) + (i == 3 ? 1 : 0)];
    }
    if (i >= 3) {
      break;
    }
  }

  return ret.join("");
};

// Human-readable labels for VolumeKind (drive classification).
export const VOLUME_KIND_LABELS: Record<string, string> = {
  Fixed: "Local disk",
  Removable: "Removable drive",
  Optical: "Optical disc",
  Network: "Network drive",
  RamDisk: "RAM disk",
  Substituted: "Substituted drive",
};
