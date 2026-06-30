import { Component, type ErrorInfo, type ReactNode } from 'react';

import { ErrorState } from '@/components/ErrorState';

type RootErrorBoundaryProps = {
  children: ReactNode;
  /** Optional hook for surfacing the error beyond the visible fallback. */
  onError?: (error: Error, info: ErrorInfo) => void;
};

type RootErrorBoundaryState = {
  error: Error | null;
};

/**
 * App-shell error boundary. A single bad event, frame, or render must never
 * white-screen the dashboard: any thrown render is caught and surfaced to the
 * visible {@link ErrorState} (no silent failure), with a recover action that
 * resets the boundary so a transient fault can be retried in place.
 */
class RootErrorBoundary extends Component<RootErrorBoundaryProps, RootErrorBoundaryState> {
  override state: RootErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): RootErrorBoundaryState {
    return { error };
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    // Surface to the caller (which routes it to visible state / telemetry).
    // The visible ErrorState below is the operator-facing surface; this hook
    // is the place to add structured reporting, never a console-only swallow.
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
            actionLabel="Reload view"
            error={error}
            onRetry={this.handleReset}
            title="The dashboard hit an unexpected error"
          />
        </div>
      );
    }

    return this.props.children;
  }
}

export { RootErrorBoundary };
