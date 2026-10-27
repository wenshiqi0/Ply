// Readable's listener-driven mode switches (a 'data' listener resumes, a
// 'readable' listener pauses) and its read-only properties.
import { Stream } from "./legacy";
import { Readable, emitReadable, flow } from "./readable";
import type { ReadableImpl } from "./readable";

type Listener = (...args: never[]) => unknown;
const base = Stream.prototype as unknown as {
  on(this: ReadableImpl, type: string | symbol, listener: Listener): ReadableImpl;
  removeListener(this: ReadableImpl, type: string | symbol, listener: Listener): ReadableImpl;
  removeAllListeners(this: ReadableImpl, type?: string | symbol): ReadableImpl;
};

function updateReadableListening(stream: ReadableImpl): void {
  const state = stream._readableState;
  state.readableListening = stream.listenerCount("readable") > 0;
  if (state.resumeScheduled && state.paused === false) state.flowing = true;
  else if (stream.listenerCount("data") > 0) stream.resume();
  else if (!state.readableListening) state.flowing = null;
}

function resumeScheduled(stream: ReadableImpl): void {
  const state = stream._readableState;
  if (state.resumeScheduled) return;
  state.resumeScheduled = true;
  process.nextTick(() => {
    if (!state.reading) stream.read(0);
    state.resumeScheduled = false;
    stream.emit("resume");
    flow(stream);
    if (state.flowing && !state.reading) stream.read(0);
  });
}

function on(this: ReadableImpl, type: string | symbol, listener: Listener): ReadableImpl {
  const result = base.on.call(this, type, listener);
  const state = this._readableState;
  if (type === "data") {
    state.readableListening = this.listenerCount("readable") > 0;
    if (state.flowing !== false) this.resume();
  } else if (type === "readable" && !state.endEmitted && !state.readableListening) {
    state.readableListening = state.needReadable = true;
    state.flowing = false;
    state.emittedReadable = false;
    if (state.length) emitReadable(this);
    else if (!state.reading) process.nextTick(() => this.read(0));
  }
  return result;
}
function removeListener(this: ReadableImpl, type: string | symbol, listener: Listener): ReadableImpl {
  const result = base.removeListener.call(this, type, listener);
  if (type === "readable") process.nextTick(updateReadableListening, this);
  return result;
}

Object.assign(Readable.prototype, {
  on,
  addListener: on,
  removeListener,
  off: removeListener,
  removeAllListeners(this: ReadableImpl, type?: string | symbol) {
    const result = base.removeAllListeners.call(this, type);
    if (type === "readable" || type === undefined) process.nextTick(updateReadableListening, this);
    return result;
  },
  resume(this: ReadableImpl) {
    const state = this._readableState;
    if (!state.flowing) { state.flowing = !state.readableListening; resumeScheduled(this); }
    state.paused = false;
    return this;
  },
  pause(this: ReadableImpl) {
    const state = this._readableState;
    if (state.flowing !== false) { state.flowing = false; this.emit("pause"); }
    state.paused = true;
    return this;
  },
});

const getters: Record<string, (this: ReadableImpl) => unknown> = {
  readableDidRead() { return this._readableState.dataEmitted; },
  readableAborted() { const s = this._readableState; return !!(s.readable !== false && (s.destroyed || s.errored) && !s.endEmitted); },
  readableHighWaterMark() { return this._readableState.highWaterMark; },
  readableBuffer() { return this._readableState.buffer; },
  readableLength() { return this._readableState.length; },
  readableObjectMode() { return this._readableState.objectMode; },
  readableEncoding() { return this._readableState.encoding; },
  readableEnded() { return this._readableState.endEmitted; },
  errored() { return this._readableState.errored; },
  closed() { return this._readableState.closed; },
};
for (const [name, get] of Object.entries(getters)) Object.defineProperty(Readable.prototype, name, { get, enumerable: false, configurable: true });
Object.defineProperties(Readable.prototype, {
  readable: {
    get(this: ReadableImpl) { const s = this._readableState; return !!s && s.readable !== false && !s.destroyed && !s.errorEmitted && !s.endEmitted; },
    set(this: ReadableImpl, value: boolean) { if (this._readableState) this._readableState.readable = !!value; },
    configurable: true,
  },
  readableFlowing: {
    get(this: ReadableImpl) { return this._readableState.flowing; },
    set(this: ReadableImpl, value: boolean | null) { if (this._readableState) this._readableState.flowing = value; },
    configurable: true,
  },
  destroyed: {
    get(this: ReadableImpl) { return this._readableState ? this._readableState.destroyed : false; },
    set(this: ReadableImpl, value: boolean) { if (this._readableState) this._readableState.destroyed = value; },
    configurable: true,
  },
});
