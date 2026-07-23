export const FRAME_BATCH_FALLBACK_MS = 50;

export type AnimationFrameBatchScheduler = {
  requestAnimationFrame(callback: () => void): number | null;
  cancelAnimationFrame(handle: number): void;
  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout>;
  clearTimeout(handle: ReturnType<typeof setTimeout>): void;
};

export type AnimationFrameBatch<T> = {
  push(item: T, flushImmediately?: boolean): void;
  flush(): void;
  cancel(): void;
};

const browserScheduler: AnimationFrameBatchScheduler = {
  requestAnimationFrame(callback) {
    if (typeof globalThis.requestAnimationFrame !== 'function') {
      return null;
    }
    return globalThis.requestAnimationFrame(callback);
  },
  cancelAnimationFrame(handle) {
    if (typeof globalThis.cancelAnimationFrame === 'function') {
      globalThis.cancelAnimationFrame(handle);
    }
  },
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle),
};

/**
 * Coalesces an ordered stream into one delivery per browser frame. Browsers pause
 * animation frames in hidden tabs, so a short timeout races the frame and wins
 * there. Whichever callback wins cancels the other one.
 */
export function createAnimationFrameBatch<T>(
  deliver: (items: readonly T[]) => void,
  scheduler: AnimationFrameBatchScheduler = browserScheduler
): AnimationFrameBatch<T> {
  let pending: T[] = [];
  let animationFrame: number | null = null;
  let fallbackTimer: ReturnType<typeof setTimeout> | null = null;
  let scheduleGeneration = 0;
  let active = true;

  function clearSchedule(): void {
    scheduleGeneration += 1;
    if (animationFrame !== null) {
      scheduler.cancelAnimationFrame(animationFrame);
      animationFrame = null;
    }
    if (fallbackTimer !== null) {
      scheduler.clearTimeout(fallbackTimer);
      fallbackTimer = null;
    }
  }

  function flush(): void {
    if (!active || pending.length === 0) {
      return;
    }

    const items = pending;
    pending = [];
    clearSchedule();
    deliver(items);
  }

  function schedule(): void {
    if (animationFrame !== null || fallbackTimer !== null) {
      return;
    }

    const generation = scheduleGeneration;
    const flushIfCurrent = () => {
      if (active && generation === scheduleGeneration) {
        flush();
      }
    };
    animationFrame = scheduler.requestAnimationFrame(flushIfCurrent);
    fallbackTimer = scheduler.setTimeout(flushIfCurrent, FRAME_BATCH_FALLBACK_MS);
  }

  return {
    push(item, flushImmediately = false) {
      if (!active) {
        return;
      }
      pending.push(item);
      if (flushImmediately) {
        flush();
      } else {
        schedule();
      }
    },
    flush,
    cancel() {
      if (!active) {
        return;
      }
      active = false;
      pending = [];
      clearSchedule();
    },
  };
}
