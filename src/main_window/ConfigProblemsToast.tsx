import { commands, type ConfigProblem, type PaneHandle } from "../lib/bindings";
import { safe } from "../lib/ipc";
import type { PreferencesState } from "../lib/preferences";
import styles from "./ConfigProblemsToast.module.scss";

/// Keeps focus where it is: the toast sits over the panes, and a click on
/// it shouldn't pull focus out of them.
const keepFocus = (e: React.MouseEvent) => e.preventDefault();

/// `location: message`, with the message's `quoted` names as code.
export function ProblemText({ problem }: { problem: ConfigProblem }) {
  return (
    <>
      <code>{problem.location}</code>:{" "}
      {problem.message
        .split("`")
        .map((part, i) => (i % 2 ? <code key={i}>{part}</code> : part))}
    </>
  );
}

/// What's wrong in settings.toml, over the main window until dismissed or
/// fixed. The same list, with lines, is at the top of Settings.
export function ConfigProblemsToast({
  preferences,
  paneHandle,
}: {
  preferences: PreferencesState | null;
  paneHandle: PaneHandle;
}) {
  const problems = preferences?.problems ?? [];
  if (problems.length === 0 || preferences?.problems_dismissed) return null;
  const [first] = problems;
  return (
    <div className={styles.toast} role="alert">
      <span className={styles.mark} aria-hidden>
        ⚠
      </span>
      <div className={styles.text}>
        <div className={styles.title}>
          {problems.length === 1
            ? "settings.toml has a problem"
            : `settings.toml has ${problems.length} problems`}
        </div>
        <div className={styles.detail}>
          {first.line !== null && `Line ${first.line} · `}
          <ProblemText problem={first} />
          {problems.length > 1 && ` (and ${problems.length - 1} more)`}
        </div>
      </div>
      <button
        type="button"
        onMouseDown={keepFocus}
        onClick={() => safe(commands.cmdOpenSettings(paneHandle))}
      >
        Show
      </button>
      <button
        type="button"
        onMouseDown={keepFocus}
        onClick={() => safe(commands.openConfigFile())}
      >
        Open settings.toml
      </button>
      <button
        type="button"
        className={styles.dismiss}
        onMouseDown={keepFocus}
        onClick={() => void commands.dismissConfigProblems()}
        aria-label="Dismiss"
        title="Dismiss until settings.toml changes"
      >
        ×
      </button>
    </div>
  );
}
