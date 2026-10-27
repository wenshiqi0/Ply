// Destroying a stream and reporting its errors, in Node's order: the error
// event, then close, each on the next tick.
import type { Base, Callback } from "./state";

const kDestroy = Symbol("kDestroy");

function checkError(error: Error | null | undefined, stream: Base): void {
  if (!error) return;
  const r = stream._readableState, w = stream._writableState;
  if (w && !w.errored) w.errored = error;
  if (r && !r.errored) r.errored = error;
}

export function emitErrorNT(stream: Base, error: Error): void {
  const r = stream._readableState, w = stream._writableState;
  if (w?.errorEmitted || r?.errorEmitted) return;
  if (w) w.errorEmitted = true;
  if (r) r.errorEmitted = true;
  stream.emit("error", error);
}

export function emitCloseNT(stream: Base): void {
  const r = stream._readableState, w = stream._writableState;
  if (w) w.closeEmitted = true;
  if (r) r.closeEmitted = true;
  if (w?.emitClose || r?.emitClose) stream.emit("close");
}

function destroyNow(stream: Base, error: Error | null | undefined, callback?: Callback): void {
  let called = false;
  const done = (error?: Error | null) => {
    if (called) return;
    called = true;
    checkError(error, stream);
    const r = stream._readableState, w = stream._writableState;
    if (w) w.closed = true;
    if (r) r.closed = true;
    if (typeof callback === "function") callback(error);
    if (error) process.nextTick(() => { emitErrorNT(stream, error); emitCloseNT(stream); });
    else process.nextTick(emitCloseNT, stream);
  };
  try {
    stream._destroy(error || null, done);
  } catch (thrown) {
    done(thrown as Error);
  }
}

/** Readable#destroy and Writable#destroy. */
export function destroy(this: Base, error?: Error | null, callback?: Callback): Base {
  const r = this._readableState, w = this._writableState;
  if (w?.destroyed || r?.destroyed) {
    if (typeof callback === "function") callback();
    return this;
  }
  checkError(error, this);
  if (w) w.destroyed = true;
  if (r) r.destroyed = true;
  if ((w ?? r)?.constructed === false) this.once(kDestroy, (late: Error | null) => destroyNow(this, error ?? late, callback));
  else destroyNow(this, error, callback);
  return this;
}

/** Destroys with `error` when the stream destroys itself on errors; otherwise only reports it. */
export function errorOrDestroy(stream: Base, error: Error, sync = false): void {
  const r = stream._readableState, w = stream._writableState;
  if (w?.destroyed || r?.destroyed) return;
  if (r?.autoDestroy || w?.autoDestroy) stream.destroy(error);
  else if (error) {
    if (w && !w.errored) w.errored = error;
    if (r && !r.errored) r.errored = error;
    if (sync) process.nextTick(emitErrorNT, stream, error); else emitErrorNT(stream, error);
  }
}

/** Runs `_construct` on the next tick; reads and writes wait for it. */
export function construct(stream: Base, afterwards: () => void): void {
  if (typeof stream._construct !== "function") return;
  const r = stream._readableState, w = stream._writableState;
  if (r) r.constructed = false;
  if (w) w.constructed = false;
  stream.once(kConstruct, afterwards);
  if (stream.listenerCount(kConstruct) > 1) return;
  process.nextTick(() => {
    let called = false;
    const done = (error?: Error | null) => {
      if (called) { errorOrDestroy(stream, Object.assign(new Error("Callback called multiple times"), { code: "ERR_MULTIPLE_CALLBACK" })); return; }
      called = true;
      if (r) r.constructed = true;
      if (w) w.constructed = true;
      if (r?.destroyed || w?.destroyed) stream.emit(kDestroy, error);
      else if (error) errorOrDestroy(stream, error, true);
      else stream.emit(kConstruct);
    };
    try { stream._construct!(done); } catch (error) { done(error as Error); }
  });
}
const kConstruct = Symbol("kConstruct");

/** Destroys anything stream-like, as pipeline and finished do. */
export function destroyer(stream: unknown, error?: Error | null): void {
  if (!stream || typeof stream !== "object") return;
  const target = stream as Partial<Base> & { abort?: () => void; close?: () => void };
  if (typeof target.destroy === "function") target.destroy(error);
  else if (typeof target.close === "function") target.close();
}
