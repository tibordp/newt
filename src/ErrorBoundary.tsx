import React from "react";
import { useRouteError } from "react-router-dom";
import styles from "./ErrorBoundary.module.scss";

function ErrorDisplay({
  error,
  onRetry,
}: {
  error: unknown;
  onRetry?: () => void;
}) {
  const message = error instanceof Error ? error.message : String(error);
  const stack = error instanceof Error ? error.stack : undefined;

  return (
    <div className={styles.container}>
      <h2>Something went wrong</h2>
      <pre className={styles.message}>{message}</pre>
      {stack && <pre className={styles.stack}>{stack}</pre>}
      {onRetry && (
        <button className={styles.retryButton} onClick={onRetry}>
          Try again
        </button>
      )}
    </div>
  );
}

/**
 * Route-level error element for react-router.
 * Catches errors that react-router intercepts before they reach the class boundary.
 */
export function RouteErrorBoundary() {
  const error = useRouteError();
  return (
    <ErrorDisplay error={error} onRetry={() => window.location.reload()} />
  );
}

/**
 * Top-level class-based error boundary for errors outside the router.
 */
interface State {
  error: Error | null;
}

export class ErrorBoundary extends React.Component<
  React.PropsWithChildren,
  State
> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  render() {
    if (this.state.error) {
      return (
        <ErrorDisplay
          error={this.state.error}
          onRetry={() => this.setState({ error: null })}
        />
      );
    }

    return this.props.children;
  }
}
