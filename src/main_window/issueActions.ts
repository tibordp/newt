import type { IssueAction } from "../lib/bindings";

export const ACTION_LABELS: Record<IssueAction, string> = {
  skip: "Skip",
  overwrite: "Overwrite",
  overwrite_if_newer: "Overwrite if newer",
  overwrite_if_size_differs: "Overwrite if size differs",
  overwrite_if_size_or_date_differs: "Overwrite if size or date differs",
  retry: "Retry",
};

/// Conditional overwrites, offered from a menu on the Overwrite button.
export const OVERWRITE_VARIANTS: IssueAction[] = [
  "overwrite_if_newer",
  "overwrite_if_size_differs",
  "overwrite_if_size_or_date_differs",
];

/// Answers the copy/move dialog can give to conflicts up front.
export const CONFLICT_RESOLUTIONS: IssueAction[] = [
  "skip",
  "overwrite",
  ...OVERWRITE_VARIANTS,
];
