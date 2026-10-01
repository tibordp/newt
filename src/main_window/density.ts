import type { Density } from "../lib/bindings";

/// File list row height in px per density, mirroring the `--row-height`
/// values in `_tokens.scss`. The pane's virtualization arithmetic needs the
/// number itself, so it is stated here rather than read back off computed
/// style; the two must stay in step or rows and spacers drift apart.
export const ROW_HEIGHT: Record<Density, number> = {
  compact: 20,
  comfortable: 22,
  spacious: 24,
};

export function rowHeightFor(density: Density | undefined): number {
  return ROW_HEIGHT[density ?? "comfortable"];
}
