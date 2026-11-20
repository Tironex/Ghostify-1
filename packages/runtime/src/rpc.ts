/** JSON-RPC 2.0 client for the Ghostify bridge with authentication, reconnect and timeouts. */

export interface RpcErrorBody {
  code: number;
  message: string;
  data?: unknown;
}

export class RpcError extends Error {
  readonly code: number;
  readonly data: unknown;
  constructor(body: RpcErrorBody) {
    super(body.message);
    this.name = "RpcError";
    this.code = body.code;
    this.data = body.data;
  }
}

export type ConnectionState = "connecting" | "open" | "closed";

export interface SocketLike {
  readyState: number;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: unknown) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  send(data: string): void;
  close(): void;
}

export type SocketFactory = (url: string) => SocketLike;

export interface ClientOptions {
  /** Candidate endpoints; the client rotates through them while connecting. */
  urls: string[];
  secret: string;
  socketFactory?: SocketFactory;
  requestTimeoutMs?: number;
  minBackoffMs?: number;
  maxBackoffMs?: number;
  /** Replaceable for tests. */
  random?: () => number;
}

type Pending = {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
};

type NotificationHandler = (params: unknown) => void;

const OPEN = 1;

export class BridgeClient {
  private readonly options: Required<Omit<ClientOptions, "socketFactory">> & { socketFactory: SocketFactory };
  private socket: SocketLike | null = null;
  private urlIndex = 0;
  private attempt = 0;
  private nextId = 1;
  private authed = false;
  private stopped = true;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly pending = new Map<number, Pending>();
  private readonly handlers = new Map<string, Set<NotificationHandler>>();
  private readonly stateHandlers = new Set<(state: ConnectionState) => void>();
  private state: ConnectionState = "closed";

  constructor(options: ClientOptions) {
    if (options.urls.length === 0) throw new Error("at least one bridge url is required");
    this.options = {
      requestTimeoutMs: 10_000,
      minBackoffMs: 250,
      maxBackoffMs: 15_000,
      random: Math.random,
      socketFactory: (url) => new WebSocket(url) as unknown as SocketLike,
      ...options,
    };
  }

  get connectionState(): ConnectionState {
    return this.state;
  }

  /** Starts connecting. Reconnects forever until {@link stop} is called. */
  start(): void {
    if (!this.stopped) return;
    this.stopped = false;
    this.open();
  }

  stop(): void {
    this.stopped = true;
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    const socket = this.socket;
    this.socket = null;
    this.setState("closed");
    this.failAll(new Error("bridge client stopped"));
    if (socket) {
      detach(socket);
      try {
        socket.close();
      } catch {
        /* already closed */
      }
    }
  }

  onState(handler: (state: ConnectionState) => void): () => void {
    this.stateHandlers.add(handler);
    return () => this.stateHandlers.delete(handler);
  }

  /** Subscribes to server notifications. Returns the unsubscribe function. */
  on(method: string, handler: NotificationHandler): () => void {
    let set = this.handlers.get(method);
    if (!set) {
      set = new Set();
      this.handlers.set(method, set);
    }
    set.add(handler);
    return () => set.delete(handler);
  }

  call<T = unknown>(method: string, params: unknown = null): Promise<T> {
    const socket = this.socket;
    if (!socket || !this.authed || socket.readyState !== OPEN) {
      return Promise.reject(new Error("bridge is not connected"));
    }
    return this.send<T>(socket, method, params);
  }

  private send<T>(socket: SocketLike, method: string, params: unknown): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`bridge call "${method}" timed out`));
      }, this.options.requestTimeoutMs);
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject, timer });
      try {
        socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      } catch (error) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  }

  private open(): void {
    const url = this.options.urls[this.urlIndex % this.options.urls.length] as string;
    this.setState("connecting");
    let socket: SocketLike;
    try {
      socket = this.options.socketFactory(url);
    } catch {
      this.scheduleReconnect();
      return;
    }
    this.socket = socket;
    this.authed = false;

    socket.onopen = () => {
      // The secret is only ever sent as the first message, never in the URL.
      this.send(socket, "auth", { secret: this.options.secret }).then(
        () => {
          if (this.socket !== socket) return;
          this.authed = true;
          this.attempt = 0;
          this.setState("open");
        },
        () => {
          if (this.socket === socket) socket.close();
        },
      );
    };
    socket.onmessage = (event) => this.receive(event.data);
    const lost = () => {
      if (this.socket !== socket) return;
      this.socket = null;
      this.authed = false;
      this.failAll(new Error("bridge connection lost"));
      this.setState("closed");
      if (!this.stopped) this.scheduleReconnect();
    };
    socket.onclose = lost;
    socket.onerror = () => {
      // An error is always followed by close; closing explicitly covers odd implementations.
      try {
        socket.close();
      } catch {
        /* ignore */
      }
    };
  }

  private scheduleReconnect(): void {
    if (this.stopped || this.reconnectTimer !== null) return;
    this.urlIndex += 1;
    const { minBackoffMs, maxBackoffMs, random } = this.options;
    const ceiling = Math.min(maxBackoffMs, minBackoffMs * 2 ** this.attempt);
    this.attempt += 1;
    // Full jitter keeps many clients from reconnecting in lockstep.
    const delay = Math.max(minBackoffMs, Math.floor(random() * ceiling));
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      if (!this.stopped) this.open();
    }, delay);
  }

  private receive(data: unknown): void {
    if (typeof data !== "string") return;
    let message: { id?: unknown; method?: unknown; params?: unknown; result?: unknown; error?: RpcErrorBody };
    try {
      message = JSON.parse(data);
    } catch {
      return;
    }
    if (typeof message !== "object" || message === null) return;
    if (typeof message.id === "number") {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      clearTimeout(pending.timer);
      if (message.error) pending.reject(new RpcError(message.error));
      else pending.resolve(message.result);
      return;
    }
    if (typeof message.method === "string") {
      for (const handler of [...(this.handlers.get(message.method) ?? [])]) {
        try {
          handler(message.params);
        } catch (error) {
          console.error("[ghostify] notification handler failed", error);
        }
      }
    }
  }

  private failAll(error: Error): void {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  private setState(state: ConnectionState): void {
    if (this.state === state) return;
    this.state = state;
    for (const handler of [...this.stateHandlers]) handler(state);
  }
}

function detach(socket: SocketLike): void {
  socket.onopen = null;
  socket.onmessage = null;
  socket.onclose = null;
  socket.onerror = null;
}
