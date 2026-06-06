import * as grpc from "@grpc/grpc-js";
import type {
  ActivityError,
  ActivityId,
  ActivityTask as WireActivityTask,
  Payload as WirePayload,
  WorkerProtocolClient,
  WorkerToServer,
} from "./proto/index.js";

export type WorkerIdentity = string;

export interface ReconnectConfig {
  readonly initialDelayMs: number;
  readonly maxDelayMs: number;
  readonly maxAttempts: number;
}

export interface WorkerConfig {
  readonly endpoint: string;
  readonly taskQueue: string;
  readonly identity: WorkerIdentity;
  readonly maxConcurrency: number;
  readonly credentials?: grpc.ChannelCredentials;
  readonly channelOptions?: grpc.ChannelOptions;
  readonly reconnect?: ReconnectConfig;
}

export interface Payload {
  readonly contentType: string;
  readonly bytes: Uint8Array;
}

export type WorkflowId = string;
export type ActivityIdKey = string;

export interface ActivityTask {
  readonly workflowId: WorkflowId;
  readonly activityId: ActivityIdKey;
  readonly activityType: string;
  readonly input: Payload;
  readonly attempt: number;
}

export interface ActivityFailure {
  readonly retryable: boolean;
  readonly message: string;
  readonly details?: Payload;
}

export interface WorkerRegistration {
  readonly taskQueue: string;
  readonly identity: WorkerIdentity;
  readonly activityTypes: readonly string[];
}

export type WorkerSessionEvent =
  | { readonly kind: "task"; readonly task: ActivityTask }
  | { readonly kind: "closed" };

export interface WorkerSession {
  handshake(config: WorkerConfig): Promise<void>;
  register(activityTypes: readonly string[]): Promise<void>;
  receiveTasks(): AsyncIterable<WorkerSessionEvent>;
  reportResult(
    workflowId: WorkflowId,
    activityId: ActivityIdKey,
    result: Payload,
  ): Promise<void>;
  reportFailure(
    workflowId: WorkflowId,
    activityId: ActivityIdKey,
    failure: ActivityFailure,
  ): Promise<void>;
  sendHeartbeat(
    workflowId: WorkflowId,
    activityId: ActivityIdKey,
    progress?: Payload,
  ): Promise<void>;
  close(): Promise<void>;
}

export type WorkerSessionFactory = (
  config: WorkerConfig,
) => Promise<WorkerSession>;

export interface GrpcClientFactory {
  create(
    endpoint: string,
    credentials: grpc.ChannelCredentials,
    options?: grpc.ChannelOptions,
  ): WorkerProtocolClient;
}

export class GrpcWorkerSession implements WorkerSession {
  private readonly stream: ReturnType<WorkerProtocolClient["streamWorker"]>;
  private registration?: WorkerRegistration;

  public constructor(client: WorkerProtocolClient) {
    this.stream = client.streamWorker();
  }

  public async handshake(config: WorkerConfig): Promise<void> {
    validateWorkerConfig(config);
    this.registration = {
      taskQueue: config.taskQueue,
      identity: config.identity,
      activityTypes: [],
    };
  }

  public async register(activityTypes: readonly string[]): Promise<void> {
    const registeredTypes = validateActivityTypes(activityTypes);
    const registration = this.registration;
    if (registration === undefined) {
      throw new Error("worker session must handshake before register");
    }
    this.registration = { ...registration, activityTypes: registeredTypes };
    await this.write({
      register: {
        namespace: registration.taskQueue,
        activityTypes: registeredTypes,
      },
    });
  }

  public async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
    for await (const message of this.stream) {
      if (message.task !== undefined) {
        yield { kind: "task", task: decodeTask(message.task) };
      }
    }
    yield { kind: "closed" };
  }

  public async reportResult(
    workflowId: WorkflowId,
    activityId: ActivityIdKey,
    result: Payload,
  ): Promise<void> {
    await this.write({
      result: {
        workflowId: encodeWorkflowId(workflowId),
        activityId: encodeActivityId(activityId),
        result: encodePayload(result),
      },
    });
  }

  public async reportFailure(
    workflowId: WorkflowId,
    activityId: ActivityIdKey,
    failure: ActivityFailure,
  ): Promise<void> {
    await this.write({
      result: {
        workflowId: encodeWorkflowId(workflowId),
        activityId: encodeActivityId(activityId),
        error: encodeFailure(failure),
      },
    });
  }

  public async sendHeartbeat(
    workflowId: WorkflowId,
    activityId: ActivityIdKey,
    progress?: Payload,
  ): Promise<void> {
    await this.write({
      heartbeat: {
        workflowId: encodeWorkflowId(workflowId),
        activityId: encodeActivityId(activityId),
        progress: progress === undefined ? undefined : encodePayload(progress),
      },
    });
  }

  public async close(): Promise<void> {
    this.stream.end();
  }

  private async write(message: WorkerToServer): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      this.stream.write(message, (error?: Error | null) => {
        if (error === undefined || error === null) {
          resolve();
        } else {
          reject(error);
        }
      });
    });
  }
}

export async function connectGrpcWorkerSession(
  config: WorkerConfig,
  clientFactory: GrpcClientFactory,
): Promise<WorkerSession> {
  validateWorkerConfig(config);
  const credentials = config.credentials ?? grpc.credentials.createInsecure();
  const client = clientFactory.create(
    config.endpoint,
    credentials,
    config.channelOptions,
  );
  return new GrpcWorkerSession(client);
}

function validateWorkerConfig(config: WorkerConfig): void {
  if (config.endpoint.length === 0) {
    throw new Error("worker endpoint must not be empty");
  }
  if (config.taskQueue.length === 0) {
    throw new Error("worker taskQueue must not be empty");
  }
  if (config.identity.length === 0) {
    throw new Error("worker identity must not be empty");
  }
  if (!Number.isInteger(config.maxConcurrency) || config.maxConcurrency <= 0) {
    throw new Error("worker maxConcurrency must be a positive integer");
  }
}

function validateActivityTypes(activityTypes: readonly string[]): readonly string[] {
  if (activityTypes.length === 0) {
    throw new Error("worker registration must include at least one activity type");
  }
  const normalizedTypes = [...activityTypes];
  for (const activityType of normalizedTypes) {
    if (activityType.length === 0) {
      throw new Error("worker activity type must not be empty");
    }
  }
  return normalizedTypes;
}

export function decodeTask(task: WireActivityTask): ActivityTask {
  if (task.workflowId === undefined || task.workflowId.uuid.length === 0) {
    throw new Error("activity task is missing workflow_id");
  }
  if (task.activityId === undefined) {
    throw new Error("activity task is missing activity_id");
  }
  if (task.activityType.length === 0) {
    throw new Error("activity task is missing activity_type");
  }
  if (task.input === undefined) {
    throw new Error("activity task is missing input payload");
  }
  return {
    workflowId: task.workflowId.uuid,
    activityId: activityIdToKey(task.activityId),
    activityType: task.activityType,
    input: decodePayload(task.input),
    attempt: 1,
  };
}

export function decodePayload(payload: WirePayload): Payload {
  if (payload.contentType.length === 0) {
    throw new Error("payload is missing content_type");
  }
  return {
    contentType: payload.contentType,
    bytes: Uint8Array.from(payload.bytes),
  };
}

export function encodePayload(payload: Payload): WirePayload {
  if (payload.contentType.length === 0) {
    throw new Error("payload contentType must not be empty");
  }
  return {
    contentType: payload.contentType,
    bytes: Uint8Array.from(payload.bytes),
  };
}

export function activityIdToKey(activityId: ActivityId): ActivityIdKey {
  return String(activityId.sequencePosition);
}

function encodeWorkflowId(workflowId: WorkflowId): { readonly uuid: string } {
  if (workflowId.length === 0) {
    throw new Error("workflowId must not be empty");
  }
  return { uuid: workflowId };
}

function encodeActivityId(
  activityId: ActivityIdKey,
): { readonly sequencePosition: string } {
  if (activityId.length === 0) {
    throw new Error("activityId must not be empty");
  }
  return { sequencePosition: activityId };
}

function encodeFailure(failure: ActivityFailure): ActivityError {
  if (failure.message.length === 0) {
    throw new Error("activity failure message must not be empty");
  }
  return {
    kind: failure.retryable ? 1 : 2,
    message: failure.message,
    details:
      failure.details === undefined ? undefined : encodePayload(failure.details),
  };
}
