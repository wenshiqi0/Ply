// stream.Duplex, Transform and PassThrough (RFC 0015). A Duplex inherits
// Readable and borrows Writable's methods, keeping both states, as in Node.
import { Stream } from "./legacy";
import { Readable, maybeReadMore } from "./readable";
import type { ReadableImpl, ReadableOptions } from "./readable";
import { Writable, bindOnwrite, clearBuffer, finishMaybe } from "./writable";
import type { WritableImpl, WritableOptions } from "./writable";
import { construct } from "./destroy";
import { ReadableState, WritableState, errors, kinds } from "./state";
import type { Callback, Chunk } from "./state";

export interface DuplexOptions extends ReadableOptions, Omit<WritableOptions, keyof ReadableOptions> {
  readable?: boolean;
  writable?: boolean;
}
export type DuplexImpl = ReadableImpl & WritableImpl & { allowHalfOpen: boolean };

export function Duplex(this: DuplexImpl, options?: DuplexOptions): DuplexImpl | void {
  if (!(this instanceof Duplex)) return new (Duplex as unknown as new (options?: DuplexOptions) => DuplexImpl)(options);
  const r = this._readableState = new ReadableState(options, true);
  const w = this._writableState = new WritableState(options, true);
  bindOnwrite(this, w);
  this.allowHalfOpen = options?.allowHalfOpen !== false;
  if (options) {
    if (options.readable === false) { r.readable = false; r.ended = true; r.endEmitted = true; }
    if (options.writable === false) { w.writable = false; w.ending = true; w.ended = true; w.finished = true; }
    if (typeof options.read === "function") this._read = options.read as DuplexImpl["_read"];
    if (typeof options.write === "function") this._write = options.write as DuplexImpl["_write"];
    if (typeof options.writev === "function") this._writev = options.writev as DuplexImpl["_writev"];
    if (typeof options.destroy === "function") this._destroy = options.destroy as DuplexImpl["_destroy"];
    if (typeof options.final === "function") this._final = options.final as DuplexImpl["_final"];
    if (typeof options.construct === "function") this._construct = options.construct as DuplexImpl["_construct"];
    if (options.signal) kinds.addAbortSignal?.(options.signal, this);
  }
  Stream.call(this, options);
  construct(this, () => {
    if (r.needReadable) maybeReadMore(this, r);
    if (!w.writing) clearBuffer(this, w);
    finishMaybe(this, w);
  });
}
Object.setPrototypeOf(Duplex.prototype, Readable.prototype);
Object.setPrototypeOf(Duplex, Readable);
kinds.Duplex = Duplex as unknown as new () => object;

for (const name of Object.keys(Writable.prototype)) {
  const own = Duplex.prototype as unknown as Record<string, unknown>;
  if (!own[name]) own[name] = (Writable.prototype as unknown as Record<string, unknown>)[name];
}
for (const name of ["writable", "writableHighWaterMark", "writableObjectMode", "writableBuffer", "writableLength", "writableFinished",
  "writableCorked", "writableEnded", "writableNeedDrain", "writableAborted"]) {
  Object.defineProperty(Duplex.prototype, name, Object.getOwnPropertyDescriptor(Writable.prototype, name)!);
}
Object.defineProperty(Duplex.prototype, "destroyed", {
  get(this: DuplexImpl) { return !!this._readableState && !!this._writableState && this._readableState.destroyed && this._writableState.destroyed; },
  set(this: DuplexImpl, value: boolean) { if (this._readableState && this._writableState) { this._readableState.destroyed = value; this._writableState.destroyed = value; } },
  configurable: true,
});

export interface TransformOptions extends DuplexOptions {
  transform?(this: TransformImpl, chunk: Chunk, encoding: BufferEncoding, callback: (error?: Error | null, data?: Chunk) => void): void;
  flush?(this: TransformImpl, callback: (error?: Error | null, data?: Chunk) => void): void;
}
type TransformImpl = DuplexImpl & {
  _transform(chunk: Chunk, encoding: string, callback: (error?: Error | null, data?: Chunk) => void): void;
  _flush?(callback: (error?: Error | null, data?: Chunk) => void): void;
  pending: Callback | null;
};

export function Transform(this: TransformImpl, options?: TransformOptions): TransformImpl | void {
  if (!(this instanceof Transform)) return new (Transform as unknown as new (options?: TransformOptions) => TransformImpl)(options);
  // A zero readable high water mark would stall; Node keeps the writable one.
  const readableHighWaterMark = options?.highWaterMark ?? options?.readableHighWaterMark;
  if (options && readableHighWaterMark === 0) options = { ...options, highWaterMark: undefined, readableHighWaterMark: 0, writableHighWaterMark: options.writableHighWaterMark || 0 };
  Duplex.call(this, options);
  this._readableState.sync = false;
  this.pending = null;
  if (options) {
    if (typeof options.transform === "function") this._transform = options.transform as TransformImpl["_transform"];
    if (typeof options.flush === "function") this._flush = options.flush as TransformImpl["_flush"];
  }
  this.on("prefinish", function (this: TransformImpl) { if (this._final !== final) final.call(this); });
}
Object.setPrototypeOf(Transform.prototype, Duplex.prototype);
Object.setPrototypeOf(Transform, Duplex);

function final(this: TransformImpl, callback?: Callback): void {
  if (typeof this._flush === "function" && !this.destroyed) {
    this._flush((error, data) => {
      if (error) { if (callback) callback(error); else this.destroy(error); return; }
      if (data !== null && data !== undefined) this.push(data);
      this.push(null);
      callback?.();
    });
  } else {
    this.push(null);
    callback?.();
  }
}

Object.assign(Transform.prototype, {
  _final: final,
  _transform(): void { throw errors.notImplemented("_transform()"); },
  _write(this: TransformImpl, chunk: Chunk, encoding: string, callback: Callback) {
    const r = this._readableState, w = this._writableState;
    const length = r.length;
    this._transform(chunk, encoding, (error, value) => {
      if (error) { callback(error); return; }
      if (value !== null && value !== undefined) this.push(value);
      if (r.ended) process.nextTick(callback);
      else if (w.ended || length === r.length || r.length < r.highWaterMark) callback();
      else this.pending = callback;
    });
  },
  _read(this: TransformImpl) {
    if (this.pending) { const callback = this.pending; this.pending = null; callback(); }
  },
});

export function PassThrough(this: TransformImpl, options?: TransformOptions): TransformImpl | void {
  if (!(this instanceof PassThrough)) return new (PassThrough as unknown as new (options?: TransformOptions) => TransformImpl)(options);
  Transform.call(this, options);
}
Object.setPrototypeOf(PassThrough.prototype, Transform.prototype);
Object.setPrototypeOf(PassThrough, Transform);
PassThrough.prototype._transform = function (chunk: Chunk, _encoding: string, callback: (error: null, data: Chunk) => void) { callback(null, chunk); };
