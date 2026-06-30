import { Component, type ErrorInfo, type ReactNode } from 'react';

import { ErrorState } from '@/components/ErrorState';

type FailoverErrorBoundaryProps = {
  children: ReactNode;
  /** Optional hook for surfacing the error beyond the visible fallback. */
  onError?: (error: Error, info: ErrorInfo) => void;
};

type FailoverErrorBoundaryState = {
  error: Error | null;
};

/**
 * Failover-view error boundary. The demo runs live on stage: a single unexpected
 * event variant or a bad frame during the kill must NEVER white-screen. Any thrown
 * render is caught and surfaced to the visible {@link ErrorState} (no silent
 * failure — the operator sees exactly what broke), with a recover action that
 * resets the boundary so a transient fault can be retried in place.
 */
class FailoverErrorBoundary extends Component<
  FailoverErrorBoundaryProps,
  FailoverErrorBoundaryState
> {
  override state: FailoverErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): FailoverErrorBoundaryState {
    return { error };
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    // Surface to the caller; the visible ErrorState below is the operator-facing
    // surface. This hook is for structured reporting, never a console-only swallow.
    this.props.onError?.(error, info);
  }

  private readonly handleReset = (): void => {
    this.setState({ error: null });
  };

  override render(): ReactNode {
    const { error } = this.state;

    if (error !== null) {
      return (
        <div className="p-6">
          <ErrorState
            actionLabel="Recover view"
            error={error}
            message={`The failover view hit an unexpected error and was caught before it could white-screen the demo. ${error.message}`}
            onRetry={this.handleReset}
            title="Failover view error (caught)"
          />
        </div>
      );
    }

    return this.props.children;
  }
}

export { FailoverErrorBoundary };
