import type { IssueAction, IssueKind } from "../lib/bindings";

export const ACTION_LABELS: Record<IssueAction, string> = {
  skip: "Skip",
  overwrite: "Overwrite",
  merge: "Merge",
  overwrite_if_newer: "Overwrite if newer",
  overwrite_if_size_differs: "Overwrite if size differs",
  overwrite_if_size_or_date_differs: "Overwrite if size or date differs",
  retry: "Retry",
  delete_permanently: "Delete Permanently",
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

/// How an answer reads for an issue: a folder is replaced, not overwritten.
export function actionLabel(action: IssueAction, kind: IssueKind): string {
  if (action === "overwrite" && kind === "FolderExists") return "Replace";
  return ACTION_LABELS[action] ?? action;
}
