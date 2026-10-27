// stream.Writable (RFC 0015): Node's write buffering, drain, final,
// prefinish and finish, in Node's order.
import { Stream } from "./legacy";
import { construct, destroy, errorOrDestroy } from "./destroy";
import { WritableState, errors, kinds, nop } from "./state";
import type { Base, Buffered, Callback, Chunk, StreamOptions } from "./state";

export interface WritableOptions extends StreamOptions {
  write?(this: WritableImpl, chunk: Chunk, encoding: BufferEncoding, callback: Callback): void;
  writev?(this: WritableImpl, chunks: { chunk: Chunk; encoding: BufferEncoding }[], callback: Callback): void;
  final?(this: WritableImpl, callback: Callback): void;
  destroy?(this: WritableImpl, error: Error | null, callback: Callback): void;
  construct?(this: WritableImpl, callback: Callback): void;
}
export interface WritableImpl extends Base {
  _writableState: WritableState;
  _write(chunk: Chunk, encoding: string, callback: Callback): void;
  _writev?(chunks: Buffered[], callback: Callback): void;
  _final?(callback: Callback): void;
}

export function Writable(this: WritableImpl, options?: WritableOptions): WritableImpl | void {
  const duplex = kinds.Duplex !== undefined && this instanceof kinds.Duplex;
  if (!duplex && !Function.prototype[Symbol.hasInstance].call(Writable, this)) {
    return new (Writable as unknown as new (options?: WritableOptions) => WritableImpl)(options);
  }
  const state = this._writableState = new WritableState(options, duplex);
  bindOnwrite(this, state);
  if (options) {
    if (typeof options.write === "function") this._write = options.write as WritableImpl["_write"];
    if (typeof options.writev === "function") this._writev = options.writev as WritableImpl["_writev"];
    if (typeof options.destroy === "function") this._destroy = options.destroy;
    if (typeof options.final === "function") this._final = options.final;
    if (typeof options.construct === "function") this._construct = options.construct;
    if (options.signal) kinds.addAbortSignal?.(options.signal, this);
  }
  Stream.call(this, options);
  construct(this, () => { if (!state.writing) clearBuffer(this, state); finishMaybe(this, state); });
}
Object.setPrototypeOf(Writable.prototype, Stream.prototype);
Object.setPrototypeOf(Writable, Stream);
// A Duplex is a Writable too, though it inherits from Readable.
Object.defineProperty(Writable, Symbol.hasInstance, {
  value(this: unknown, object: unknown) {
    if (Function.prototype[Symbol.hasInstance].call(this, object)) return true;
    if (this !== Writable) return false;
    return !!object && (object as Base)._writableState instanceof WritableState;
  },
});

function write(stream: WritableImpl, chunk: Chunk, encoding: unknown, callback: unknown): boolean | Error {
  const state = stream._writableState;
  let cb = typeof callback === "function" ? (callback as Callback) : nop;
  let enc = encoding as BufferEncoding | "buffer" | undefined;
  if (typeof encoding === "function") { cb = encoding as Callback; enc = state.defaultEncoding; }
  else if (!enc) enc = state.defaultEncoding;
  else if (enc !== "buffer" && !Buffer.isEncoding(enc)) throw errors.unknownEncoding(enc);
  if (chunk === null) throw errors.nullValues();
  if (!state.objectMode) {
    if (typeof chunk === "string") { if (state.decodeStrings) { chunk = Buffer.from(chunk, enc as BufferEncoding); enc = "buffer"; } }
    else if (chunk instanceof Buffer) enc = "buffer";
    else if (chunk instanceof Uint8Array) { chunk = Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength); enc = "buffer"; }
    else throw errors.chunkType(chunk);
  }
  const error = state.ending ? errors.writeAfterEnd() : state.destroyed ? errors.destroyed("write") : null;
  if (error) {
    process.nextTick(cb, error);
    errorOrDestroy(stream, error, true);
    return error;
  }
  state.pendingcb++;
  const length = state.objectMode ? 1 : (chunk as { length: number }).length;
  state.length += length;
  const ok = state.length < state.highWaterMark;
  if (!ok) state.needDrain = true;
  if (state.writing || state.corked || state.errored || !state.constructed) {
    state.buffered.push({ chunk, encoding: enc, callback: cb });
    if (state.allBuffers && enc !== "buffer") state.allBuffers = false;
    if (state.allNoop && cb !== nop) state.allNoop = false;
  } else doWrite(stream, state, false, length, chunk, enc, cb);
  return ok && !state.errored && !state.destroyed;
}

function doWrite(stream: WritableImpl, state: WritableState, writev: boolean, length: number, chunk: unknown, encoding: string, callback: Callback): void {
  state.writelen = length;
  state.writecb = callback;
  state.writing = true;
  state.sync = true;
  if (state.destroyed) state.onwrite(errors.destroyed("write"));
  else if (writev) stream._writev!(chunk as Buffered[], state.onwrite);
  else stream._write(chunk, encoding, state.onwrite);
  state.sync = false;
}

/** The callback _write calls, bound to its stream; Duplex binds it too. */
export function bindOnwrite(stream: WritableImpl, state: WritableState): void {
  state.onwrite = (error?: Error | null) => onwrite(stream, error);
}

function onwrite(stream: WritableImpl, error?: Error | null): void {
  const state = stream._writableState;
  const sync = state.sync;
  const cb = state.writecb;
  if (typeof cb !== "function") { errorOrDestroy(stream, errors.multipleCallback()); return; }
  state.writing = false;
  state.writecb = null;
  state.length -= state.writelen;
  state.writelen = 0;
  if (error) {
    if (!state.errored) state.errored = error;
    const r = stream._readableState;
    if (r && !r.errored) r.errored = error;
    const fail = () => { state.pendingcb--; cb(error); errorBuffer(state); errorOrDestroy(stream, error); };
    if (sync) process.nextTick(fail); else fail();
    return;
  }
  if (state.buffered.length > state.bufferedIndex) clearBuffer(stream, state);
  if (!sync) { afterWrite(stream, state, 1, cb); return; }
  if (state.afterWriteTickInfo !== null && state.afterWriteTickInfo.cb === cb) state.afterWriteTickInfo.count++;
  else {
    const info = state.afterWriteTickInfo = { count: 1, cb };
    process.nextTick(() => { state.afterWriteTickInfo = null; afterWrite(stream, state, info.count, info.cb); });
  }
}

function afterWrite(stream: WritableImpl, state: WritableState, count: number, cb: Callback): void {
  if (!state.ending && !stream.destroyed && state.length === 0 && state.needDrain) { state.needDrain = false; stream.emit("drain"); }
  while (count-- > 0) { state.pendingcb--; cb(null); }
  if (state.destroyed) errorBuffer(state);
  finishMaybe(stream, state);
}

function errorBuffer(state: WritableState): void {
  if (state.writing) return;
  for (let n = state.bufferedIndex; n < state.buffered.length; ++n) {
    const entry = state.buffered[n]!;
    state.length -= state.objectMode ? 1 : (entry.chunk as { length: number }).length;
    entry.callback(state.errored ?? errors.destroyed("write"));
  }
  for (const cb of state.onFinished.splice(0)) cb(state.errored ?? errors.destroyed("end"));
  state.buffered = [];
  state.bufferedIndex = 0;
  state.allBuffers = true;
  state.allNoop = true;
}

export function clearBuffer(stream: WritableImpl, state: WritableState): void {
  if (state.corked || state.bufferProcessing || state.destroyed || !state.constructed) return;
  const { buffered, bufferedIndex, objectMode } = state;
  const count = buffered.length - bufferedIndex;
  if (!count) return;
  let i = bufferedIndex;
  state.bufferProcessing = true;
  if (count > 1 && stream._writev) {
    state.pendingcb -= count - 1;
    const callback: Callback = state.allNoop ? nop : (error) => { for (let n = i; n < buffered.length; ++n) buffered[n]!.callback(error); };
    const chunks = Object.assign(buffered.slice(i) as Buffered[], { allBuffers: state.allBuffers });
    doWrite(stream, state, true, state.length, chunks, "", callback);
    state.buffered = [];
    state.bufferedIndex = 0;
    state.allBuffers = true;
    state.allNoop = true;
  } else {
    do {
      const { chunk, encoding, callback } = buffered[i]!;
      buffered[i++] = null;
      doWrite(stream, state, false, objectMode ? 1 : (chunk as { length: number }).length, chunk, encoding, callback);
    } while (i < buffered.length && !state.writing);
    if (i === buffered.length) { state.buffered = []; state.bufferedIndex = 0; state.allBuffers = true; state.allNoop = true; }
    else if (i > 256) { buffered.splice(0, i); state.bufferedIndex = 0; }
    else state.bufferedIndex = i;
  }
  state.bufferProcessing = false;
}

export const needFinish = (s: WritableState) => s.ending && !s.destroyed && s.constructed && s.length === 0 && !s.errored &&
  s.buffered.length === 0 && !s.finished && !s.writing && !s.errorEmitted && !s.closeEmitted;

function prefinish(stream: WritableImpl, state: WritableState): void {
  if (state.prefinished || state.finalCalled) return;
  if (typeof stream._final !== "function" || state.destroyed) { state.prefinished = true; stream.emit("prefinish"); return; }
  state.finalCalled = true;
  let called = false;
  const onFinal = (error?: Error | null) => {
    if (called) { errorOrDestroy(stream, error ?? errors.multipleCallback()); return; }
    called = true;
    state.pendingcb--;
    if (error) {
      for (const cb of state.onFinished.splice(0)) cb(error);
      errorOrDestroy(stream, error, state.sync);
    } else if (needFinish(state)) {
      state.prefinished = true;
      stream.emit("prefinish");
      state.pendingcb++;
      process.nextTick(finish, stream, state);
    }
  };
  state.sync = true;
  state.pendingcb++;
  try { stream._final(onFinal); } catch (error) { onFinal(error as Error); }
  state.sync = false;
}

export function finishMaybe(stream: WritableImpl, state: WritableState, sync = false): void {
  if (!needFinish(state)) return;
  prefinish(stream, state);
  if (state.pendingcb !== 0) return;
  if (sync) {
    state.pendingcb++;
    process.nextTick(() => { if (needFinish(state)) finish(stream, state); else state.pendingcb--; });
  } else if (needFinish(state)) {
    state.pendingcb++;
    finish(stream, state);
  }
}

function finish(stream: WritableImpl, state: WritableState): void {
  state.pendingcb--;
  state.finished = true;
  for (const cb of state.onFinished.splice(0)) cb(null);
  stream.emit("finish");
  if (state.autoDestroy) {
    const r = stream._readableState;
    if (!r || (r.autoDestroy && (r.endEmitted || r.readable === false))) stream.destroy();
  }
}

Object.assign(Writable.prototype, {
  destroy,
  _destroy(error: Error | null, callback: Callback) { callback(error); },
  _write(this: WritableImpl, chunk: Chunk, encoding: string, callback: Callback) {
    if (this._writev) this._writev([{ chunk, encoding: encoding as BufferEncoding, callback }], callback);
    else throw errors.notImplemented("_write()");
  },
  write(this: WritableImpl, chunk: Chunk, encoding?: unknown, callback?: unknown) { return write(this, chunk, encoding, callback) === true; },
});
export { write as writeChunk };
