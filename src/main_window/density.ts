import type { Density, PlatformStyle } from "../lib/bindings";
import { fontSize, useAppearance } from "../lib/appearance";

/// Room around a file list row's text at the default density: 22px rows at
/// each look's default font size (Explorer's 12px, everywhere else 13px).
const ROW_PADDING: Record<PlatformStyle, number> = {
  macos: 9,
  windows: 10,
  gnome: 9,
  kde: 9,
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
