// stream.Readable (RFC 0015): Node's buffering and flow algorithm, so
// 'readable', 'data', 'end' and 'close' arrive in Node's order.
import { StringDecoder } from "../string_decoder";
import { Stream } from "./legacy";
import { construct, destroy, errorOrDestroy } from "./destroy";
import { ReadableState, errors, kinds, nop } from "./state";
import type { Base, Callback, Chunk, StreamOptions } from "./state";

export interface ReadableOptions extends StreamOptions {
  read?(this: ReadableImpl, size: number): void;
  destroy?(this: ReadableImpl, error: Error | null, callback: Callback): void;
  construct?(this: ReadableImpl, callback: Callback): void;
}
export interface ReadableImpl extends Base {
  _readableState: ReadableState;
  _read(size: number): void;
  read(n?: number): Chunk | null;
  push(chunk: Chunk, encoding?: BufferEncoding): boolean;
  pause(): ReadableImpl;
  resume(): ReadableImpl;
  readable: boolean;
  allowHalfOpen?: boolean;
  writable?: boolean;
  unpipe(dest?: unknown): ReadableImpl;
}

export function Readable(this: ReadableImpl, options?: ReadableOptions): ReadableImpl | void {
  if (!(this instanceof Readable)) return new (Readable as unknown as new (options?: ReadableOptions) => ReadableImpl)(options);
  const duplex = kinds.Duplex !== undefined && this instanceof kinds.Duplex;
  this._readableState = new ReadableState(options, duplex);
  if (options) {
    if (typeof options.read === "function") this._read = options.read;
    if (typeof options.destroy === "function") this._destroy = options.destroy;
    if (typeof options.construct === "function") this._construct = options.construct;
    if (options.signal && !duplex) kinds.addAbortSignal?.(options.signal, this);
  }
  Stream.call(this, options);
  construct(this, () => { if (this._readableState.needReadable) maybeReadMore(this, this._readableState); });
}
Object.setPrototypeOf(Readable.prototype, Stream.prototype);
Object.setPrototypeOf(Readable, Stream);

function toChunk(state: ReadableState, chunk: Chunk, encoding: BufferEncoding | undefined, front: boolean): [Chunk, string] | Error {
  if (state.objectMode) return [chunk, encoding ?? ""];
  if (typeof chunk === "string") {
    const from = encoding || state.defaultEncoding;
    if (state.encoding === from) return [chunk, from];
    if (front && state.encoding) return [Buffer.from(chunk, from).toString(state.encoding), state.encoding];
    return [Buffer.from(chunk, from), ""];
  }
  if (chunk instanceof Uint8Array) return [chunk instanceof Buffer ? chunk : Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength), ""];
  if (chunk === null || chunk === undefined) return [chunk, ""];
  return errors.chunkType(chunk);
}

function addChunk(stream: ReadableImpl, state: ReadableState, chunk: Chunk, front: boolean): void {
  if (state.flowing && state.length === 0 && !state.sync && stream.listenerCount("data") > 0) {
    if (state.multiAwaitDrain) (state.awaitDrainWriters as Set<unknown>).clear(); else state.awaitDrainWriters = null;
    state.dataEmitted = true;
    stream.emit("data", chunk);
  } else {
    state.length += state.objectMode ? 1 : (chunk as { length: number }).length;
    if (front) state.buffer.unshift(chunk); else state.buffer.push(chunk);
    if (state.needReadable) emitReadable(stream);
  }
  maybeReadMore(stream, state);
}

function addFromSource(stream: ReadableImpl, raw: Chunk, encoding: BufferEncoding | undefined, front: boolean): boolean {
  const state = stream._readableState;
  const converted = toChunk(state, raw, encoding, front);
  if (converted instanceof Error) { errorOrDestroy(stream, converted); return false; }
  let [chunk] = converted;
  const [, chunkEncoding] = converted;
  if (chunk === null) {
    state.reading = false;
    onEofChunk(stream, state);
  } else if (state.objectMode || (chunk as { length: number }).length > 0) {
    if (front) {
      if (state.endEmitted) errorOrDestroy(stream, errors.unshiftAfterEnd());
      else if (state.destroyed || state.errored) return false;
      else addChunk(stream, state, chunk, true);
    } else if (state.ended) errorOrDestroy(stream, errors.pushAfterEof());
    else if (state.destroyed || state.errored) return false;
    else {
      state.reading = false;
      if (state.decoder && !chunkEncoding) {
        chunk = state.decoder.write(chunk as Buffer);
        if (state.objectMode || (chunk as string).length !== 0) addChunk(stream, state, chunk, false); else maybeReadMore(stream, state);
      } else addChunk(stream, state, chunk, false);
    }
  } else if (!front) {
    state.reading = false;
    maybeReadMore(stream, state);
  }
  return !state.ended && (state.length < state.highWaterMark || state.length === 0);
}

function onEofChunk(stream: ReadableImpl, state: ReadableState): void {
  if (state.ended) return;
  if (state.decoder) {
    const rest = state.decoder.end();
    if (rest.length) { state.buffer.push(rest); state.length += state.objectMode ? 1 : rest.length; }
  }
  state.ended = true;
  if (state.sync) emitReadable(stream);
  else { state.needReadable = false; state.emittedReadable = true; emitReadableNT(stream); }
}

export function emitReadable(stream: ReadableImpl): void {
  const state = stream._readableState;
  state.needReadable = false;
  if (!state.emittedReadable) { state.emittedReadable = true; process.nextTick(emitReadableNT, stream); }
}
function emitReadableNT(stream: ReadableImpl): void {
  const state = stream._readableState;
  if (!state.destroyed && !state.errored && (state.length || state.ended)) { stream.emit("readable"); state.emittedReadable = false; }
  state.needReadable = !state.flowing && !state.ended && state.length <= state.highWaterMark;
  flow(stream);
}

export function maybeReadMore(stream: ReadableImpl, state: ReadableState): void {
  if (state.readingMore || !state.constructed) return;
  state.readingMore = true;
  process.nextTick(() => {
    while (!state.reading && !state.ended && (state.length < state.highWaterMark || (state.flowing && state.length === 0))) {
      const length = state.length;
      stream.read(0);
      if (length === state.length) break;
    }
    state.readingMore = false;
  });
}

function howMuchToRead(n: number, state: ReadableState): number {
  if (n <= 0 || (state.length === 0 && state.ended)) return 0;
  if (state.objectMode) return 1;
  if (Number.isNaN(n)) return state.flowing && state.length ? (state.buffer[0] as { length: number }).length : state.length;
  if (n <= state.length) return n;
  return state.ended ? state.length : 0;
}

/** Takes n bytes (or one object) from the buffer, joining or splitting chunks. */
function fromList(n: number, state: ReadableState): Chunk | null {
  if (state.length === 0) return null;
  if (state.objectMode) return state.buffer.shift();
  if (!n || n >= state.length) {
    const all = state.buffer.splice(0);
    return state.decoder ? all.join("") : all.length === 1 ? all[0] : Buffer.concat(all as Buffer[], state.length);
  }
  const first = state.buffer[0] as Buffer | string;
  if (n < first.length) {
    state.buffer[0] = first.slice(n);
    return first.slice(0, n);
  }
  if (n === first.length) return state.buffer.shift();
  const parts: (Buffer | string)[] = [];
  let need = n;
  while (need > 0) {
    const head = state.buffer[0] as Buffer | string;
    if (head.length <= need) { parts.push(head); state.buffer.shift(); need -= head.length; }
    else { parts.push(head.slice(0, need)); state.buffer[0] = head.slice(need); need = 0; }
  }
  return state.decoder ? parts.join("") : Buffer.concat(parts as Buffer[], n);
}

export function endReadable(stream: ReadableImpl): void {
  const state = stream._readableState;
  if (state.endEmitted) return;
  state.ended = true;
  process.nextTick(() => {
    if (state.errored || state.closeEmitted || state.endEmitted || state.length !== 0) return;
    state.endEmitted = true;
    stream.emit("end");
    if (stream.writable && stream.allowHalfOpen === false) process.nextTick(() => (stream as unknown as { end(): void }).end());
    else if (state.autoDestroy) {
      const w = stream._writableState;
      if (!w || (w.autoDestroy && (w.finished || w.writable === false))) stream.destroy();
    }
  });
}

export function flow(stream: ReadableImpl): void {
  const state = stream._readableState;
  while (state.flowing && stream.read() !== null);
}

const proto = {
  destroy,
  _destroy(error: Error | null, callback: Callback) { callback(error); },
  _read(): void { throw errors.notImplemented("_read()"); },
  push(this: ReadableImpl, chunk: Chunk, encoding?: BufferEncoding) { return addFromSource(this, chunk, encoding, false); },
  unshift(this: ReadableImpl, chunk: Chunk, encoding?: BufferEncoding) { return addFromSource(this, chunk, encoding, true); },
  isPaused(this: ReadableImpl) { const s = this._readableState; return s.paused === true || s.flowing === false; },
  setEncoding(this: ReadableImpl, encoding: BufferEncoding) {
    const state = this._readableState;
    const decoder = new StringDecoder(encoding);
    state.decoder = decoder;
    state.encoding = decoder.encoding;
    const content = state.buffer.splice(0).map((data) => decoder.write(data as Buffer)).join("");
    if (content !== "") state.buffer.push(content);
    state.length = content.length;
    return this;
  },
  read(this: ReadableImpl, n?: number): Chunk | null {
    const state = this._readableState;
    n = n === undefined ? NaN : Number.isInteger(n) ? n : parseInt(String(n), 10);
    const original = n;
    if (n > state.highWaterMark) state.highWaterMark = 2 ** Math.ceil(Math.log2(n));
    if (n !== 0) state.emittedReadable = false;
    if (n === 0 && state.needReadable && ((state.highWaterMark !== 0 ? state.length >= state.highWaterMark : state.length > 0) || state.ended)) {
      if (state.length === 0 && state.ended) endReadable(this); else emitReadable(this);
      return null;
    }
    n = howMuchToRead(n, state);
    if (n === 0 && state.ended) {
      if (state.length === 0) endReadable(this);
      return null;
    }
    const doRead = state.needReadable || state.length === 0 || state.length - n < state.highWaterMark;
    if (doRead && !(state.ended || state.reading || state.destroyed || state.errored || !state.constructed)) {
      state.reading = true;
      state.sync = true;
      if (state.length === 0) state.needReadable = true;
      try { this._read(state.highWaterMark); } catch (error) { errorOrDestroy(this, error as Error); }
      state.sync = false;
      if (!state.reading) n = howMuchToRead(original, state);
    }
    let result: Chunk | null = n > 0 ? fromList(n, state) : null;
    if (result === null) { state.needReadable = state.length <= state.highWaterMark; n = 0; }
    else {
      state.length -= n;
      if (state.multiAwaitDrain) (state.awaitDrainWriters as Set<unknown>).clear(); else state.awaitDrainWriters = null;
    }
    if (state.length === 0) {
      if (!state.ended) state.needReadable = true;
      if (original !== n && state.ended) endReadable(this);
    }
    if (result !== null && !state.errorEmitted && !state.closeEmitted) { state.dataEmitted = true; this.emit("data", result); }
    if (result === undefined) result = null;
    return result;
  },
};
Object.assign(Readable.prototype, proto);
export { nop };
