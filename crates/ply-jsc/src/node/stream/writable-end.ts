// Writable#end, corking, and Writable's properties.
import { Writable, clearBuffer, finishMaybe, writeChunk } from "./writable";
import type { WritableImpl } from "./writable";
import { errors } from "./state";
import type { Callback, Chunk } from "./state";

function end(this: WritableImpl, chunk?: Chunk | Callback, encoding?: BufferEncoding | Callback, callback?: Callback): WritableImpl {
  const state = this._writableState;
  if (typeof chunk === "function") { callback = chunk as Callback; chunk = null; encoding = undefined; }
  else if (typeof encoding === "function") { callback = encoding; encoding = undefined; }
  let error: Error | undefined;
  if (chunk !== null && chunk !== undefined) {
    const result = writeChunk(this, chunk, encoding, undefined);
    if (result instanceof Error) error = result;
  }
  if (state.corked) { state.corked = 1; uncork.call(this); }
  if (error) { /* reported by write */ }
  else if (!state.errored && !state.ending) {
    state.ending = true;
    finishMaybe(this, state, true);
    state.ended = true;
  } else if (state.finished) error = errors.alreadyFinished("end");
  else if (state.destroyed) error = errors.destroyed("end");
  if (typeof callback === "function") {
    if (error) process.nextTick(callback, error);
    else if (state.finished) process.nextTick(callback, null);
    else state.onFinished.push(callback);
  }
  return this;
}

function uncork(this: WritableImpl): void {
  const state = this._writableState;
  if (state.corked) {
    state.corked--;
    if (!state.writing) clearBuffer(this, state);
  }
}

Object.assign(Writable.prototype, {
  end,
  uncork,
  cork(this: WritableImpl) { this._writableState.corked++; },
  setDefaultEncoding(this: WritableImpl, encoding: string) {
    const name = typeof encoding === "string" ? encoding.toLowerCase() : encoding;
    if (!Buffer.isEncoding(name)) throw errors.unknownEncoding(encoding);
    this._writableState.defaultEncoding = name;
    return this;
  },
});

const getters: Record<string, (this: WritableImpl) => unknown> = {
  writableFinished() { return this._writableState ? this._writableState.finished : false; },
  writableObjectMode() { return this._writableState ? this._writableState.objectMode : false; },
  writableBuffer() { return this._writableState.buffered.slice(this._writableState.bufferedIndex); },
  writableEnded() { return this._writableState ? this._writableState.ending : false; },
  writableNeedDrain() { const s = this._writableState; return s ? !s.destroyed && !s.ending && s.needDrain : false; },
  writableHighWaterMark() { return this._writableState?.highWaterMark; },
  writableCorked() { return this._writableState ? this._writableState.corked : 0; },
  writableLength() { return this._writableState?.length; },
  writableAborted() { const s = this._writableState; return !!(s.writable !== false && (s.destroyed || s.errored) && !s.finished); },
  errored() { return this._writableState ? this._writableState.errored : null; },
  closed() { return this._writableState ? this._writableState.closed : false; },
};
for (const [name, get] of Object.entries(getters)) Object.defineProperty(Writable.prototype, name, { get, configurable: true });
Object.defineProperties(Writable.prototype, {
  writable: {
    get(this: WritableImpl) { const s = this._writableState; return !!s && s.writable !== false && !s.destroyed && !s.errored && !s.ending && !s.ended; },
    set(this: WritableImpl, value: boolean) { if (this._writableState) this._writableState.writable = !!value; },
    configurable: true,
  },
  destroyed: {
    get(this: WritableImpl) { return this._writableState ? this._writableState.destroyed : false; },
    set(this: WritableImpl, value: boolean) { if (this._writableState) this._writableState.destroyed = value; },
    configurable: true,
  },
});
