import type {
  ActivityFailure,
  ActivityIdKey,
  Payload,
  ReconnectConfig,
  WorkerConfig,
  WorkerSession,
  WorkerSessionFactory,
  WorkflowId,
} from "./session.js";

export type PendingActivityReport =
  | {
      readonly kind: "completed";
      readonly workflowId: WorkflowId;
      readonly activityId: ActivityIdKey;
      readonly result: Payload;
    }
  | {
      readonly kind: "failed";
      readonly workflowId: WorkflowId;
      readonly activityId: ActivityIdKey;
      readonly failure: ActivityFailure;
    };

export class UnackedResultTracker {
  private readonly reports = new Map<ActivityIdKey, PendingActivityReport>();

  public record(report: PendingActivityReport): void {
    this.reports.set(report.activityId, report);
  }

  public acknowledge(activityId: ActivityIdKey): void {
    this.reports.delete(activityId);
  }

  public get(activityId: ActivityIdKey): PendingActivityReport | undefined {
    return this.reports.get(activityId);
  }

  public len(): number {
    return this.reports.size;
  }

  public isEmpty(): boolean {
    return this.reports.size === 0;
  }

  public snapshot(): readonly PendingActivityReport[] {
    return [...this.reports.values()];
  }
}

export interface ReconnectDependencies {
  readonly createSession: WorkerSessionFactory;
  readonly sleep?: (delayMs: number) => Promise<void>;
  readonly logger?: WorkerLogger;
}

export interface WorkerLogger {
  info(message: string, fields?: Record<string, unknown>): void;
  warn(message: string, fields?: Record<string, unknown>): void;
  error(message: string, fields?: Record<string, unknown>): void;
}

export async function reconnectWithBackoff(
  config: WorkerConfig,
  activityTypes: readonly string[],
  dependencies: ReconnectDependencies,
): Promise<WorkerSession> {
  const reconnect = requireReconnectConfig(config.reconnect);
  let delayMs = reconnect.initialDelayMs;
  let attempt = 1;
  let lastError: unknown;

  while (attempt <= reconnect.maxAttempts) {
    try {
      dependencies.logger?.info("worker reconnect attempt", { attempt });
      const session = await dependencies.createSession(config);
      await session.handshake(config);
      await session.register(activityTypes);
      dependencies.logger?.info("worker reconnect succeeded", { attempt });
      return session;
    } catch (error) {
      lastError = error;
      dependencies.logger?.warn("worker reconnect attempt failed", { attempt });
      if (attempt === reconnect.maxAttempts) {
        break;
      }
      await (dependencies.sleep ?? defaultSleep)(delayMs);
      delayMs = nextDelay(delayMs, reconnect.maxDelayMs);
      attempt += 1;
    }
  }

  throw new Error("worker reconnect attempts exhausted", { cause: lastError });
}

export async function reReportUnacked(
  session: WorkerSession,
  tracker: UnackedResultTracker,
  logger?: WorkerLogger,
): Promise<void> {
  for (const report of tracker.snapshot()) {
    logger?.info("worker re-reporting unacknowledged activity", {
      activityId: report.activityId,
      kind: report.kind,
    });
    if (report.kind === "completed") {
      await session.reportResult(
        report.workflowId,
        report.activityId,
        report.result,
      );
    } else {
      await session.reportFailure(
        report.workflowId,
        report.activityId,
        report.failure,
      );
    }
  }
}

export function requireReconnectConfig(
  reconnect: ReconnectConfig | undefined,
): ReconnectConfig {
  if (reconnect === undefined) {
    throw new Error("worker reconnect config is required");
  }
  if (reconnect.initialDelayMs <= 0) {
    throw new Error("worker reconnect initialDelayMs must be greater than zero");
  }
  if (reconnect.maxDelayMs <= 0) {
    throw new Error("worker reconnect maxDelayMs must be greater than zero");
  }
  if (reconnect.maxAttempts <= 0) {
    throw new Error("worker reconnect maxAttempts must be greater than zero");
  }
  return reconnect;
}

function nextDelay(delayMs: number, maxDelayMs: number): number {
  return Math.min(delayMs * 2, maxDelayMs);
}

async function defaultSleep(delayMs: number): Promise<void> {
  await new Promise<void>((resolve) => {
    setTimeout(resolve, delayMs);
  });
}
