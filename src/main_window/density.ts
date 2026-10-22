import type { Density, PlatformStyle } from "../lib/bindings";
import { fontSize, useAppearance } from "../lib/appearance";

/// Room around a file list row's text at the default density: Finder's 22px
/// rows at 13px, Explorer's 22px at 12px, roomier rows on GNOME and KDE.
const ROW_PADDING: Record<PlatformStyle, number> = {
  macos: 9,
  windows: 10,
  gnome: 11,
  kde: 10,
};

const DENSITY_STEP: Record<Density, number> = {
  compact: -2,
  comfortable: 0,
  spacious: 2,
};

/// File list row height in px. MainWindow publishes it as `--row-height`,
/// and the pane's virtualization arithmetic uses the same number.
export function useRowHeight(density: Density | undefined): number {
  const appearance = useAppearance();
  return (
    Math.round(fontSize(appearance) + ROW_PADDING[appearance.platform]) +
    DENSITY_STEP[density ?? "comfortable"]
  );
}
