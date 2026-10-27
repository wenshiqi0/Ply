// Readable#pipe and #unpipe: data events into dest.write, pausing until
// every slow destination drains, and unpiping on end, close or finish.
import { Readable } from "./readable";
import type { ReadableImpl } from "./readable";
import { errorOrDestroy } from "./destroy";
import type { Writable } from "./state";

type Dest = Writable & { prependListener?(type: string, listener: (...args: never[]) => unknown): unknown };

function pipe(this: ReadableImpl, dest: Dest, options?: { end?: boolean }): Dest {
  return pipeInto(this, dest, options);
}

function pipeInto(src: ReadableImpl, dest: Dest, options?: { end?: boolean }): Dest {
  const state = src._readableState;
  state.pipes.push(dest);
  const ends = options?.end !== false && (dest as unknown) !== process.stdout && (dest as unknown) !== process.stderr;
  let ondrain: (() => void) | null = null;
  let cleanedUp = false;
  const unpipe = () => { src.unpipe(dest); };
  const onend = () => { dest.end(); };
  const endFn = ends ? onend : unpipe;
  const ondata = (chunk: unknown) => { if (dest.write(chunk) === false) pause(); };
  const onerror = (error: Error) => {
    unpipe();
    dest.removeListener("error", onerror);
    if (dest.listenerCount("error") === 0) {
      const s = dest._writableState ?? dest._readableState;
      if (s && !s.errorEmitted) errorOrDestroy(dest, error); else dest.emit("error", error);
    }
  };
  const onclose = () => { dest.removeListener("finish", onfinish); unpipe(); };
  const onfinish = () => { dest.removeListener("close", onclose); unpipe(); };
  const onunpipe = (readable: unknown, info?: { hasUnpiped: boolean }) => {
    if (readable === src && info && info.hasUnpiped === false) { info.hasUnpiped = true; cleanup(); }
  };
  function cleanup() {
    dest.removeListener("close", onclose);
    dest.removeListener("finish", onfinish);
    if (ondrain) dest.removeListener("drain", ondrain);
    dest.removeListener("error", onerror);
    dest.removeListener("unpipe", onunpipe);
    src.removeListener("end", onend);
    src.removeListener("end", unpipe);
    src.removeListener("data", ondata);
    cleanedUp = true;
    if (ondrain && state.awaitDrainWriters && (!dest._writableState || dest._writableState.needDrain)) ondrain();
  }
  function pause() {
    if (!cleanedUp) {
      if (state.pipes.length === 1 && state.pipes[0] === dest) { state.awaitDrainWriters = dest; state.multiAwaitDrain = false; }
      else if (state.pipes.length > 1 && state.pipes.includes(dest)) {
        if (!(state.awaitDrainWriters instanceof Set)) state.awaitDrainWriters = new Set(state.awaitDrainWriters ? [state.awaitDrainWriters] : []);
        state.multiAwaitDrain = true;
        state.awaitDrainWriters.add(dest);
      }
      src.pause();
    }
    if (!ondrain) {
      ondrain = () => {
        if (state.awaitDrainWriters === dest) state.awaitDrainWriters = null;
        else if (state.multiAwaitDrain && state.awaitDrainWriters instanceof Set) state.awaitDrainWriters.delete(dest);
        const waiting = state.awaitDrainWriters instanceof Set ? state.awaitDrainWriters.size : state.awaitDrainWriters ? 1 : 0;
        if (waiting === 0 && src.listenerCount("data")) src.resume();
      };
      dest.on("drain", ondrain);
    }
  }
  if (state.endEmitted) process.nextTick(endFn); else src.once("end", endFn);
  dest.on("unpipe", onunpipe);
  src.on("data", ondata);
  if (typeof dest.prependListener === "function") dest.prependListener("error", onerror); else dest.on("error", onerror);
  dest.once("close", onclose);
  dest.once("finish", onfinish);
  dest.emit("pipe", src);
  if (dest.writableNeedDrain === true) pause();
  else if (!state.flowing) src.resume();
  return dest;
}

function unpipe(this: ReadableImpl, dest?: Writable): ReadableImpl {
  const state = this._readableState;
  if (state.pipes.length === 0) return this;
  if (!dest) {
    const dests = state.pipes.splice(0);
    this.pause();
    for (const d of dests) d.emit("unpipe", this, { hasUnpiped: false });
    return this;
  }
  const index = state.pipes.indexOf(dest);
  if (index === -1) return this;
  state.pipes.splice(index, 1);
  if (state.pipes.length === 0) this.pause();
  dest.emit("unpipe", this, { hasUnpiped: false });
  return this;
}

Object.assign(Readable.prototype, { pipe, unpipe });
