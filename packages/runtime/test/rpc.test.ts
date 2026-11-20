import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { BridgeClient, RpcError } from "../src/rpc";
import { FakeSocket, flush } from "./fake";

const SECRET = "s".repeat(64);

function make(overrides = {}) {
  return new BridgeClient({
    urls: ["ws://127.0.0.1:1/", "ws://127.0.0.1:2/"],
    secret: SECRET,
    socketFactory: (url) => new FakeSocket(url),
    random: () => 1,
    ...overrides,
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  FakeSocket.reset();
});
afterEach(() => vi.useRealTimers());

describe("BridgeClient", () => {
  it("sends the secret as first message, not in the url", async () => {
    const client = make();
    client.start();
    const socket = FakeSocket.last;
    expect(socket.url).not.toContain(SECRET);
    socket.authenticate();
    await flush();
    expect(socket.sent[0]).toMatchObject({ jsonrpc: "2.0", method: "auth", params: { secret: SECRET } });
    expect(client.connectionState).toBe("open");
  });

  it("refuses calls until authenticated", async () => {
    const client = make();
    client.start();
    await expect(client.call("ping")).rejects.toThrow("not connected");
    FakeSocket.last.open();
    await expect(client.call("ping")).rejects.toThrow("not connected");
  });

  it("matches responses to calls by id and maps errors", async () => {
    const client = make();
    client.start();
    const socket = FakeSocket.last;
    socket.authenticate();
    await flush();

    const a = client.call("a");
    const b = client.call("b", { x: 1 });
    const [first, second] = socket.sent.slice(1);
    socket.reply(second!.id, "second");
    socket.fail(first!.id, -32601, "unknown method");
    await expect(b).resolves.toBe("second");
    const error = (await a.catch((e) => e)) as RpcError;
    expect(error).toBeInstanceOf(RpcError);
    expect(error.code).toBe(-32601);
    expect(second!.params).toEqual({ x: 1 });
  });

  it("times out calls that never get an answer", async () => {
    const client = make({ requestTimeoutMs: 1000 });
    client.start();
    FakeSocket.last.authenticate();
    await flush();
    const call = client.call("slow");
    const assertion = expect(call).rejects.toThrow("timed out");
    await vi.advanceTimersByTimeAsync(1001);
    await assertion;
  });

  it("delivers notifications and isolates failing handlers", async () => {
    const client = make();
    client.start();
    const socket = FakeSocket.last;
    socket.authenticate();
    await flush();
    const seen: unknown[] = [];
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    client.on("evt", () => {
      throw new Error("boom");
    });
    const off = client.on("evt", (p) => seen.push(p));
    socket.push("evt", 1);
    off();
    socket.push("evt", 2);
    expect(seen).toEqual([1]);
    expect(error).toHaveBeenCalledTimes(2); // the throwing handler ran for both pushes, the second one was isolated too
    error.mockRestore();
  });

  it("ignores garbage frames", async () => {
    const client = make();
    client.start();
    const socket = FakeSocket.last;
    socket.authenticate();
    await flush();
    socket.onmessage?.({ data: "{broken" });
    socket.onmessage?.({ data: new ArrayBuffer(2) });
    socket.onmessage?.({ data: "null" });
    socket.onmessage?.({ data: JSON.stringify({ id: 999, result: 1 }) });
    expect(client.connectionState).toBe("open");
  });

  it("fails pending calls when the connection drops and reconnects with backoff on the next url", async () => {
    const client = make({ minBackoffMs: 100, maxBackoffMs: 1000 });
    const states: string[] = [];
    client.onState((s) => states.push(s));
    client.start();
    const first = FakeSocket.last;
    first.authenticate();
    await flush();
    const pending = client.call("x");
    const assertion = expect(pending).rejects.toThrow("connection lost");
    first.close();
    await assertion;
    expect(client.connectionState).toBe("closed");

    await vi.advanceTimersByTimeAsync(99);
    expect(FakeSocket.instances).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(2);
    expect(FakeSocket.instances).toHaveLength(2);
    expect(FakeSocket.last.url).toBe("ws://127.0.0.1:2/");

    // second failure doubles the delay
    FakeSocket.last.close();
    await vi.advanceTimersByTimeAsync(150);
    expect(FakeSocket.instances).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(60);
    expect(FakeSocket.instances).toHaveLength(3);
    expect(FakeSocket.last.url).toBe("ws://127.0.0.1:1/");
    expect(states).toEqual(["connecting", "open", "closed", "connecting", "closed", "connecting"]);
  });

  it("resets the backoff after a successful connection", async () => {
    const client = make({ minBackoffMs: 100, maxBackoffMs: 10_000 });
    client.start();
    for (let i = 0; i < 3; i++) {
      FakeSocket.last.close();
      await vi.advanceTimersByTimeAsync(10_000);
    }
    FakeSocket.last.authenticate();
    await flush();
    const before = FakeSocket.instances.length;
    FakeSocket.last.close();
    await vi.advanceTimersByTimeAsync(101);
    expect(FakeSocket.instances.length).toBe(before + 1);
  });

  it("closes and retries when authentication is rejected", async () => {
    const client = make({ minBackoffMs: 50 });
    client.start();
    const socket = FakeSocket.last;
    socket.open();
    const auth = socket.sent[0]!;
    socket.fail(auth.id, -32001, "authentication required");
    await flush();
    expect(socket.readyState).toBe(3);
    expect(client.connectionState).toBe("closed");
    await vi.advanceTimersByTimeAsync(60);
    expect(FakeSocket.instances).toHaveLength(2);
  });

  it("stop() closes, rejects pending calls and prevents reconnecting", async () => {
    const client = make({ minBackoffMs: 10 });
    client.start();
    const socket = FakeSocket.last;
    socket.authenticate();
    await flush();
    const pending = client.call("x");
    const assertion = expect(pending).rejects.toThrow("stopped");
    client.stop();
    await assertion;
    expect(socket.readyState).toBe(3);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(FakeSocket.instances).toHaveLength(1);
    // can be started again
    client.start();
    expect(FakeSocket.instances).toHaveLength(2);
  });

  it("retries when the socket factory throws", async () => {
    let calls = 0;
    const client = make({
      minBackoffMs: 10,
      socketFactory: (url: string) => {
        if (++calls === 1) throw new Error("blocked by CSP");
        return new FakeSocket(url);
      },
    });
    client.start();
    await vi.advanceTimersByTimeAsync(20);
    expect(FakeSocket.instances).toHaveLength(1);
  });

  it("requires at least one url", () => {
    expect(() => new BridgeClient({ urls: [], secret: SECRET })).toThrow();
  });
});
