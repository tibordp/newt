import { useState, useEffect, useCallback } from "react";
import { safeCommand } from "../lib/ipc";

export type IssueAction = "skip" | "overwrite" | "retry" | "abort";

export type OperationIssue = {
  issue_id: number;
  kind: string;
  message: string;
  detail: string | null;
  actions: IssueAction[];
};

export type OperationState = {
  id: number;
  kind: string;
  description: string;
  total_bytes: number | null;
  total_items: number | null;
  bytes_done: number;
  items_done: number;
  current_item: string;
  status:
    | "scanning"
    | "running"
    | "completed"
    | "failed"
    | "cancelled"
    | "waiting_for_input";
  error: string | null;
  issue: OperationIssue | null;
};

function progressFraction(op: OperationState): number {
  if (op.status === "scanning") return 0;
  if (op.total_bytes !== null && op.total_bytes > 0) {
    return op.bytes_done / op.total_bytes;
  }
  if (op.total_items !== null && op.total_items > 0) {
    return op.items_done / op.total_items;
  }
  return 0;
}

function formatProgress(op: OperationState): string {
  if (op.status === "scanning") return "Scanning...";
  if (op.total_bytes !== null && op.total_bytes > 0) {
    const pct = Math.round((op.bytes_done / op.total_bytes) * 100);
    return `${pct}%`;
  }
  if (op.total_items !== null && op.total_items > 0) {
    return `${op.items_done}/${op.total_items}`;
  }
  return "";
}

const ACTION_LABELS: Record<IssueAction, string> = {
  skip: "Skip",
  overwrite: "Overwrite",
  retry: "Retry",
  abort: "Abort",
};

function IssueResolution({
  op,
}: {
  op: OperationState;
}) {
  const [applyToAll, setApplyToAll] = useState(false);
  const issue = op.issue!;

  const resolve = useCallback(
    (action: IssueAction) => {
      safeCommand("resolve_issue", {
        operationId: op.id,
        issueId: issue.issue_id,
        action,
        applyToAll,
      });
    },
    [op.id, issue.issue_id, applyToAll]
  );

  return (
    <div className="issue-resolution">
      <span className="issue-message">{issue.message}</span>
      <div className="issue-actions">
        {issue.actions.map((action) => (
          <button key={action} onClick={() => resolve(action)}>
            {ACTION_LABELS[action] || action}
          </button>
        ))}
        <label className="apply-to-all">
          <input
            type="checkbox"
            checked={applyToAll}
            onChange={(e) => setApplyToAll(e.target.checked)}
          />
          All
        </label>
      </div>
    </div>
  );
}

function OperationRow({ op }: { op: OperationState }) {
  const isActive =
    op.status === "scanning" ||
    op.status === "running" ||
    op.status === "waiting_for_input";
  const isWaiting = op.status === "waiting_for_input" && op.issue;

  useEffect(() => {
    if (op.status === "completed" || op.status === "cancelled") {
      safeCommand("dismiss_operation", { operationId: op.id });
    }
  }, [op.status, op.id]);

  return (
    <div className="operation-row">
      <div className="operation-info">
        <span className="operation-kind">{op.kind}</span>
        <span className="operation-description">{op.description}</span>
      </div>

      {isWaiting ? (
        <IssueResolution op={op} />
      ) : (
        <div className="operation-progress">
          {(op.status === "scanning" || op.status === "running") && (
            <>
              <div className="progress-bar">
                <div
                  className="progress-fill"
                  style={{ width: `${progressFraction(op) * 100}%` }}
                />
              </div>
              <span className="progress-text">{formatProgress(op)}</span>
            </>
          )}
          {op.status === "completed" && (
            <span className="operation-status-done">Completed</span>
          )}
          {op.status === "failed" && (
            <span className="operation-status-failed">
              Failed{op.error ? `: ${op.error}` : ""}
            </span>
          )}
          {op.status === "cancelled" && (
            <span className="operation-status-cancelled">Cancelled</span>
          )}
        </div>
      )}

      <div className="operation-actions">
        {isActive && (
          <button
            onClick={() =>
              safeCommand("cancel_operation", { operationId: op.id })
            }
          >
            Cancel
          </button>
        )}
        <button
          onClick={() =>
            safeCommand("dismiss_operation", { operationId: op.id })
          }
        >
          Dismiss
        </button>
      </div>
    </div>
  );
}

export default function OperationsPanel({
  operations,
}: {
  operations: Record<string, OperationState>;
}) {
  const ops = Object.values(operations);

  if (ops.length === 0) return null;

  return (
    <div className="operations-panel">
      {ops.map((op) => (
        <OperationRow key={op.id} op={op} />
      ))}
    </div>
  );
}
