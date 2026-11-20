import type { SocketLike } from "../src/rpc";

export class FakeSocket implements SocketLike {
  static instances: FakeSocket[] = [];
  readyState = 0;
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  sent: Array<Record<string, any>> = [];

  constructor(readonly url: string) {
    FakeSocket.instances.push(this);
  }

  send(data: string): void {
    this.sent.push(JSON.parse(data));
  }

  close(): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.onclose?.({});
  }

  // helpers driven by the tests
  open(): void {
    this.readyState = 1;
    this.onopen?.({});
  }

  reply(id: number, result: unknown): void {
    this.onmessage?.({ data: JSON.stringify({ jsonrpc: "2.0", id, result }) });
  }

  fail(id: number, code: number, message: string): void {
    this.onmessage?.({ data: JSON.stringify({ jsonrpc: "2.0", id, error: { code, message } }) });
  }

  push(method: string, params: unknown): void {
    this.onmessage?.({ data: JSON.stringify({ jsonrpc: "2.0", method, params }) });
  }

  /** Completes the auth round trip. */
  authenticate(): void {
    this.open();
    const auth = this.sent.find((m) => m.method === "auth");
    if (!auth) throw new Error("no auth message sent");
    this.reply(auth.id, { session_id: 1 });
  }

  static reset(): void {
    FakeSocket.instances = [];
  }

  static get last(): FakeSocket {
    const socket = FakeSocket.instances[FakeSocket.instances.length - 1];
    if (!socket) throw new Error("no socket created");
    return socket;
  }
}

export const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
