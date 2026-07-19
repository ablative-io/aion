export class FakeSocketFactory {
  readonly sockets: FakeSocket[] = [];
  readonly ctor: new (
    url: string
  ) => FakeSocket;

  constructor() {
    const sockets = this.sockets;

    this.ctor = class extends FakeSocket {
      constructor(url: string) {
        super(url);
        sockets.push(this);
      }
    };
  }
}

export class FakeSocket {
  readonly sent: string[] = [];
  readyState = 0;
  closeCalls = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: { wasClean: boolean }) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;

  constructor(readonly url: string) {}

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closeCalls += 1;

    if (this.readyState === 0) {
      // Mirror the browser: calling close() while CONNECTING is the illegal
      // teardown this manager must avoid. Surfacing it lets a test catch a
      // regression that re-introduces the premature close.
      throw new Error('WebSocket is closed before the connection is established');
    }

    this.readyState = 3;
    this.onclose?.({ wasClean: true });
  }

  open(): void {
    this.readyState = 1;
    this.onopen?.({} as Event);
  }

  message(data: unknown): void {
    this.onmessage?.({ data });
  }

  drop(): void {
    this.readyState = 3;
    this.onclose?.({ wasClean: false });
  }

  error(): void {
    this.readyState = 3;
    this.onerror?.({} as Event);
  }
}

export class FakeScheduler {
  readonly delays: number[] = [];
  private readonly callbacks: Array<{
    handle: ReturnType<typeof setTimeout>;
    callback: () => void;
  }> = [];
  private nextHandle = 1;

  get pendingCount(): number {
    return this.callbacks.length;
  }

  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout> {
    this.delays.push(delayMs);
    const handle = this.nextHandle as unknown as ReturnType<typeof setTimeout>;
    this.nextHandle += 1;
    this.callbacks.push({ handle, callback });
    return handle;
  }

  clearTimeout(handle: ReturnType<typeof setTimeout>): void {
    const index = this.callbacks.findIndex((entry) => entry.handle === handle);
    if (index >= 0) {
      this.callbacks.splice(index, 1);
    }
  }

  runNext(): void {
    this.callbacks.shift()?.callback();
  }
}
