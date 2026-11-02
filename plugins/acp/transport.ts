import type { Id, Message, ObjectValue } from "./types";

export class ProtocolError extends Error {
  constructor(readonly code: number, message: string) { super(message); }
}
export function object(value: unknown): ObjectValue {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new ProtocolError(-32602, "expected an object");
  return value as ObjectValue;
}
export function text(value: unknown): string {
  if (typeof value !== "string" || !value) throw new ProtocolError(-32602, "expected a non-empty string");
  return value;
}

export class Transport {
  private nextId = 0;
  private pending = new Map<string, { resolve(value: unknown): void; reject(error: Error): void; timer: ReturnType<typeof setTimeout> }>();
  private jobs = new Set<Promise<void>>();
  private incoming = new Set<Id>();
  closed = false;
  constructor(private dispatch: (method: string, params: ObjectValue) => Promise<unknown>) {}
  send(value: Omit<Message, "jsonrpc">): void {
    if (!this.closed) process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...value }) + "\n");
  }
  notify(method: string, params: ObjectValue): void { this.send({ method, params }); }
  request(method: string, params: ObjectValue): Promise<unknown> {
    const id = "ply-" + (++this.nextId);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error("client request timed out")); }, 300000);
      this.pending.set(id, { resolve, reject, timer });
      this.send({ id, method, params });
    });
  }
  cancelRequests(): void {
    for (const entry of this.pending.values()) { clearTimeout(entry.timer); entry.reject(new Error("client request cancelled")); }
    this.pending.clear();
  }
  private async accept(line: string): Promise<void> {
    let id: Id | null | undefined;
    let owned = false;
    try {
      let parsed: unknown;
      try { parsed = JSON.parse(line); } catch { throw new ProtocolError(-32700, "parse error"); }
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new ProtocolError(-32600, "invalid JSON-RPC request");
      const message = parsed as Message;
      id = message.id;
      if (message.jsonrpc !== "2.0" || (id !== undefined && id !== null && typeof id !== "string" && typeof id !== "number")) {
        id = null; throw new ProtocolError(-32600, "invalid JSON-RPC request");
      }
      if (message.method === undefined && ("result" in message || "error" in message)) {
        const entry = typeof id === "string" ? this.pending.get(id) : undefined;
        if (entry) {
          this.pending.delete(id as string); clearTimeout(entry.timer);
          if (message.error !== undefined) entry.reject(new Error("client rejected request")); else entry.resolve(message.result);
        }
        return;
      }
      if (typeof message.method !== "string") throw new ProtocolError(-32600, "request method must be a string");
      if (id === null) throw new ProtocolError(-32600, "request id must be a string or number");
      if (id !== undefined && this.incoming.has(id)) { this.send({ id, error: { code: -32600, message: "request id already running" } }); return; }
      if (id === undefined && message.method !== "session/cancel") return;
      if (id !== undefined) { this.incoming.add(id); owned = true; }
      const result = await this.dispatch(message.method, object(message.params ?? {}));
      if (id !== undefined) this.send({ id, result: result ?? {} });
    } catch (error) {
      if (id !== undefined || error instanceof ProtocolError && [-32700, -32600].includes(error.code)) {
        this.send({ id: id ?? null, error: { code: error instanceof ProtocolError ? error.code : -32603, message: error instanceof Error ? error.message : "request failed" } });
      }
    } finally { if (owned && id !== undefined && id !== null) this.incoming.delete(id); }
  }
  async run(onEnd: () => void): Promise<void> {
    let buffered = "";
    const decoder = new TextDecoder();
    try {
      for await (const chunk of process.stdin) {
        buffered += typeof chunk === "string" ? chunk : decoder.decode(chunk as Uint8Array, { stream: true });
        if (buffered.length > 16 * 1024 * 1024) throw new Error("ACP input exceeds 16 MiB");
        let end: number;
        while ((end = buffered.indexOf("\n")) >= 0) {
          const line = buffered.slice(0, end).trim(); buffered = buffered.slice(end + 1);
          if (!line) continue;
          const job = this.accept(line);
          this.jobs.add(job); void job.finally(() => this.jobs.delete(job));
        }
      }
      buffered += decoder.decode();
      if (buffered.trim()) await this.accept(buffered);
    } finally {
      onEnd(); this.cancelRequests();
      await Promise.allSettled(this.jobs); this.closed = true;
    }
  }
}
