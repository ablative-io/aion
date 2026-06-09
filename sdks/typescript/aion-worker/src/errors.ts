import type { Payload } from "./session.js";

export type FailureDetail = Payload | unknown;

export interface ActivityErrorOptions extends ErrorOptions {
	readonly details?: FailureDetail;
}

export class RetryableError extends Error {
	public readonly details?: FailureDetail;

	public constructor(message: string, options: ActivityErrorOptions = {}) {
		super(message, options);
		this.name = "RetryableError";
		this.details = options.details;
	}
}

export class TerminalError extends Error {
	public readonly details?: FailureDetail;

	public constructor(message: string, options: ActivityErrorOptions = {}) {
		super(message, options);
		this.name = "TerminalError";
		this.details = options.details;
	}
}
