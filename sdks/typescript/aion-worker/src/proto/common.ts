export interface WorkflowId {
  readonly uuid: string;
}

export interface ActivityId {
  readonly sequencePosition: bigint | number | string;
}

export interface Payload {
  readonly contentType: string;
  readonly bytes: Uint8Array;
}
