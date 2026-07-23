import { expect, test } from 'bun:test';

import {
  type AnimationFrameBatchScheduler,
  createAnimationFrameBatch,
  FRAME_BATCH_FALLBACK_MS,
} from './animationFrameBatch';

class TestBatchScheduler implements AnimationFrameBatchScheduler {
  private nextHandle = 1;
  readonly frames = new Map<number, () => void>();
  readonly timers = new Map<
    ReturnType<typeof setTimeout>,
    { callback: () => void; delayMs: number }
  >();

  requestAnimationFrame(callback: () => void): number {
    const handle = this.nextHandle;
    this.nextHandle += 1;
    this.frames.set(handle, callback);
    return handle;
  }

  cancelAnimationFrame(handle: number): void {
    this.frames.delete(handle);
  }

  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout> {
    const handle = setTimeout(() => undefined, 0);
    clearTimeout(handle);
    this.timers.set(handle, { callback, delayMs });
    return handle;
  }

  clearTimeout(handle: ReturnType<typeof setTimeout>): void {
    this.timers.delete(handle);
  }

  runFrame(): void {
    const callback = this.frames.values().next().value;
    callback?.();
  }

  runFallback(): void {
    const callback = this.timers.values().next().value?.callback;
    callback?.();
  }
}

test('frame coalescing preserves arrival order in one batch', () => {
  const scheduler = new TestBatchScheduler();
  const batches: (readonly number[])[] = [];
  const batch = createAnimationFrameBatch<number>((items) => batches.push(items), scheduler);

  batch.push(7);
  batch.push(8);
  batch.push(9);

  expect(batches).toEqual([]);
  expect([...scheduler.timers.values()].map((timer) => timer.delayMs)).toEqual([
    FRAME_BATCH_FALLBACK_MS,
  ]);
  scheduler.runFrame();
  expect(batches).toEqual([[7, 8, 9]]);
  expect(scheduler.frames.size).toBe(0);
  expect(scheduler.timers.size).toBe(0);
});

test('a terminal frame synchronously flushes earlier frames before terminal handling continues', () => {
  const scheduler = new TestBatchScheduler();
  const transitions: string[] = [];
  const batch = createAnimationFrameBatch<string>(
    (items) => transitions.push(`batch:${items.join(',')}`),
    scheduler
  );

  batch.push('started');
  batch.push('completed', true);
  transitions.push('terminal-observed');

  expect(transitions).toEqual(['batch:started,completed', 'terminal-observed']);
  expect(scheduler.frames.size).toBe(0);
  expect(scheduler.timers.size).toBe(0);
});

test('unsubscribe cancellation discards queued frames and invalidates scheduled callbacks', () => {
  const scheduler = new TestBatchScheduler();
  const batches: (readonly number[])[] = [];
  const batch = createAnimationFrameBatch<number>((items) => batches.push(items), scheduler);

  batch.push(1);
  batch.cancel();
  scheduler.runFrame();
  scheduler.runFallback();
  batch.push(2);
  batch.flush();

  expect(batches).toEqual([]);
  expect(scheduler.frames.size).toBe(0);
  expect(scheduler.timers.size).toBe(0);
});

test('the timeout fallback flushes when animation frames are paused', () => {
  const scheduler = new TestBatchScheduler();
  const batches: (readonly number[])[] = [];
  const batch = createAnimationFrameBatch<number>((items) => batches.push(items), scheduler);

  batch.push(3);
  scheduler.runFallback();

  expect(batches).toEqual([[3]]);
  expect(scheduler.frames.size).toBe(0);
  expect(scheduler.timers.size).toBe(0);
});
