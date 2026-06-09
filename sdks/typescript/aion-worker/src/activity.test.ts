import { describe, expect, it } from "vitest";
import {
	ActivityRegistry,
	decodeJsonPayload,
	defineActivity,
	encodeJsonPayload,
} from "./activity.js";
import { ActivityCancellationHandle, ActivityContext } from "./context.js";
import { RetryableError, TerminalError } from "./errors.js";
import type { ActivityFailure, ActivityTask, Payload } from "./session.js";

const textContentType = "application/vnd.aion+json";

describe("ActivityRegistry", () => {
	it("dispatches typed JSON payloads and preserves content type", async () => {
		const registry = new ActivityRegistry([
			defineActivity<{ readonly value: number }, { readonly doubled: number }>(
				"double",
				async (input) => ({ doubled: input.value * 2 }),
			),
		]);

		const outcome = await registry.dispatch(
			task("double", encodeJsonPayload({ value: 21 }, textContentType)),
		);

		expect(outcome.kind).toBe("completed");
		if (outcome.kind === "completed") {
			expect(outcome.output.contentType).toBe(textContentType);
			expect(
				decodeJsonPayload<{ readonly doubled: number }>(outcome.output),
			).toEqual({ doubled: 42 });
		}
	});

	it("rejects duplicate activity registrations", () => {
		expect(
			() =>
				new ActivityRegistry([
					defineActivity("duplicate", async () => 1),
					defineActivity("duplicate", async () => 2),
				]),
		).toThrow("activity 'duplicate' is already registered");
	});

	it("routes heartbeat only when the handler explicitly calls it", async () => {
		const sent: Payload[] = [];
		const registry = new ActivityRegistry(
			[
				defineActivity("heartbeat", async (_input, ctx) => {
					await ctx.heartbeat({ progress: 1 });
					return { ok: true };
				}),
			],
			{
				heartbeatSender: async (_workflowId, _activityId, progress) => {
					if (progress !== undefined) {
						sent.push(progress);
					}
				},
			},
		);

		expect(sent).toEqual([]);
		await registry.dispatch(task("heartbeat", encodeJsonPayload({}, textContentType)));

		expect(sent).toHaveLength(1);
		expect(sent[0]?.contentType).toBe(textContentType);
		expect(decodeJsonPayload(sent[0] as Payload)).toEqual({ progress: 1 });
	});

	it("exposes cooperative cancellation without aborting the handler", async () => {
		let observedCancelled = false;
		let handlerFinished = false;
		const registry = new ActivityRegistry([
			defineActivity("cancel", async (_input, ctx) => {
				await ctx.cancelled();
				observedCancelled = ctx.isCancelled();
				handlerFinished = true;
				return { done: true };
			}),
		]);

		const outcomePromise = registry.dispatch(
			task("cancel", encodeJsonPayload({}, textContentType)),
		);
		registry.cancelAll();
		const outcome = await outcomePromise;

		expect(observedCancelled).toBe(true);
		expect(handlerFinished).toBe(true);
		expect(outcome.kind).toBe("completed");
	});

	it("classifies explicit retryable and terminal handler errors", async () => {
		const detail = { reason: "rate-limit" };
		const retryable = new ActivityRegistry([
			defineActivity("retryable", async () => {
				throw new RetryableError("try again", { details: detail });
			}),
		]);
		const terminal = new ActivityRegistry([
			defineActivity("terminal", async () => {
				throw new TerminalError("bad input");
			}),
		]);

		const retryableOutcome = await retryable.dispatch(
			task("retryable", encodeJsonPayload({}, textContentType)),
		);
		const terminalOutcome = await terminal.dispatch(
			task("terminal", encodeJsonPayload({}, textContentType)),
		);

		expect(failure(retryableOutcome).retryable).toBe(true);
		expect(failure(retryableOutcome).message).toBe("try again");
		expect(decodeJsonPayload(failure(retryableOutcome).details as Payload)).toEqual(
			detail,
		);
		expect(failure(terminalOutcome).retryable).toBe(false);
		expect(failure(terminalOutcome).message).toBe("bad input");
	});

	it("logs unclassified handler errors as retryable warnings", async () => {
		const warnings: string[] = [];
		const registry = new ActivityRegistry(
			[
				defineActivity("unclassified", async () => {
					throw new Error("unexpected");
				}),
			],
			{
				logger: {
					info: () => {},
					warn: (message, fields) => {
						warnings.push(`${message}:${String(fields?.retryable)}`);
					},
					error: () => {},
				},
			},
		);

		const outcome = await registry.dispatch(
			task("unclassified", encodeJsonPayload({}, textContentType)),
		);

		expect(failure(outcome)).toEqual({
			retryable: true,
			message: "unexpected",
		});
		expect(warnings).toEqual([
			"activity handler threw unclassified error:true",
		]);
	});
});

describe("ActivityContext", () => {
	it("resolves cancelled waiters when the handle is cancelled", async () => {
		const handle = new ActivityCancellationHandle();
		const ctx = new ActivityContext(
			{
				workflowId: "workflow",
				activityId: "activity",
				attempt: 1,
				heartbeatSender: async () => {},
			},
			handle,
		);
		let resolved = false;
		const waiter = ctx.cancelled().then(() => {
			resolved = true;
		});

		expect(ctx.isCancelled()).toBe(false);
		handle.cancel();
		await waiter;

		expect(resolved).toBe(true);
		expect(ctx.isCancelled()).toBe(true);
	});
});

function failure(outcome: Awaited<ReturnType<ActivityRegistry["dispatch"]>>): ActivityFailure {
	if (outcome.kind !== "failed") {
		throw new Error("expected failed outcome");
	}
	return outcome.failure;
}

function task(activityType: string, input: Payload): ActivityTask {
	return {
		workflowId: "workflow",
		activityId: "activity",
		activityType,
		input,
		attempt: 1,
	};
}
