import { useState, useCallback } from "react";
import { safeCommand } from "../lib/ipc";
import * as Dialog from "@radix-ui/react-dialog";

export type IssueAction = "skip" | "overwrite" | "retry";

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
  backgrounded: boolean;
};

export function progressFraction(op: OperationState): number {
  if (op.status === "scanning") return 0;
  if (op.total_bytes !== null && op.total_bytes > 0) {
    return op.bytes_done / op.total_bytes;
  }
  if (op.total_items !== null && op.total_items > 0) {
    return op.items_done / op.total_items;
  }
  return 0;
}

export function formatProgress(op: OperationState): string {
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

export const ACTION_LABELS: Record<IssueAction, string> = {
  skip: "Skip",
  overwrite: "Overwrite",
  retry: "Retry",
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
        {issue.actions.map((action, i) => (
          <button key={action} autoFocus={i === 0} onClick={() => resolve(action)}>
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

export function OperationProgressModal({
  op,
  onCloseAutoFocus,
}: {
  op: OperationState;
  onCloseAutoFocus?: (e: Event) => void;
}) {
  const isActive =
    op.status === "scanning" ||
    op.status === "running" ||
    op.status === "waiting_for_input";
  const isWaiting = op.status === "waiting_for_input" && op.issue;

  const backgroundOp = useCallback(() => {
    safeCommand("background_operation", { operationId: op.id });
  }, [op.id]);

  const fraction = progressFraction(op);
  const progress = formatProgress(op);

  return (
    <Dialog.Root open onOpenChange={(open) => { if (!open) backgroundOp(); }}>
      <Dialog.Portal>
        <Dialog.Content
          className="operation-modal-content"
          onCloseAutoFocus={onCloseAutoFocus}
        >
          <Dialog.Title className="operation-modal-header">
            <span className="operation-modal-kind">{op.kind}</span>
            <span className="operation-modal-description">{op.description}</span>
          </Dialog.Title>

          <div className="operation-modal-body">
            {isWaiting ? (
              <IssueResolution op={op} />
            ) : (
              <>
                {(op.status === "scanning" || op.status === "running") && (
                  <>
                    <div className="operation-modal-progress-bar">
                      <div
                        className="operation-modal-progress-fill"
                        style={{ width: `${fraction * 100}%` }}
                      />
                    </div>
                    <div className="operation-modal-progress-info">
                      <span className="operation-modal-progress-text">{progress}</span>
                      {op.current_item && (
                        <span className="operation-modal-current-item" title={op.current_item}>
                          {op.current_item}
                        </span>
                      )}
                    </div>
                  </>
                )}
                {op.status === "completed" && (
                  <div className="operation-modal-status operation-modal-status-done">
                    Completed
                  </div>
                )}
                {op.status === "failed" && (
                  <div className="operation-modal-status operation-modal-status-failed">
                    Failed{op.error ? `: ${op.error}` : ""}
                  </div>
                )}
                {op.status === "cancelled" && (
                  <div className="operation-modal-status operation-modal-status-cancelled">
                    Cancelled
                  </div>
                )}
              </>
            )}
          </div>

          <div className="operation-modal-footer">
            {isActive && (
              <>
                <button
                  onClick={() => safeCommand("cancel_operation", { operationId: op.id })}
                >
                  Cancel
                </button>
                <button className="suggested" autoFocus onClick={backgroundOp}>
                  Background
                </button>
              </>
            )}
            {op.status === "failed" && (
              <button
                autoFocus
                onClick={() => safeCommand("dismiss_operation", { operationId: op.id })}
              >
                Close
              </button>
            )}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export default function OperationsPanel({
  operations,
  foregroundOperationId,
}: {
  operations: Record<string, OperationState>;
  foregroundOperationId?: number;
}) {
  const ops = Object.values(operations).filter(
    (op) => op.id !== foregroundOperationId
  );

  if (ops.length === 0) return null;

  return (
    <div className="operations-panel">
      {ops.map((op) => (
        <OperationRow key={op.id} op={op} />
      ))}
    </div>
  );
}
