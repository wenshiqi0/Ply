// stream.finished (Node's end-of-stream): calls back once a stream has
// ended, finished or failed, or closed too early.
import { errors, nop } from "./state";
import type { Base } from "./state";

type Node = Base & {
  pipe?: unknown; write?: unknown; pause?: unknown; resume?: unknown;
  readable?: boolean; writable?: boolean; writableFinished?: boolean; writableEnded?: boolean;
};
const asNode = (s: unknown) => s as Node;

export const isNodeStream = (s: unknown) => !!s && (!!asNode(s)._readableState || !!asNode(s)._writableState ||
  (typeof asNode(s).write === "function" && typeof asNode(s).on === "function") || (typeof asNode(s).pipe === "function" && typeof asNode(s).on === "function"));
export function isReadableNodeStream(s: unknown, strict = false): boolean {
  const n = asNode(s);
  return !!n && typeof n.pipe === "function" && typeof n.on === "function" && (!strict || (typeof n.pause === "function" && typeof n.resume === "function")) &&
    (!n._writableState || n._readableState?.readable !== false) && (!n._writableState || !!n._readableState);
}
export const isWritableNodeStream = (s: unknown) => !!s && typeof asNode(s).write === "function" && typeof asNode(s).on === "function" &&
  (!asNode(s)._readableState || asNode(s)._writableState?.writable !== false);

export function isReadableFinished(s: Node, strict = true): boolean | null {
  if (!isReadableNodeStream(s)) return null;
  const r = s._readableState;
  if (r?.errored) return false;
  if (typeof r?.endEmitted !== "boolean") return null;
  return !!(r.endEmitted || (!strict && r.ended && r.length === 0));
}
function isWritableFinished(s: Node, strict = true): boolean | null {
  if (!isWritableNodeStream(s)) return null;
  if (s.writableFinished === true) return true;
  const w = s._writableState;
  if (w?.errored) return false;
  if (typeof w?.finished !== "boolean") return null;
  return !!(w.finished || (!strict && w.ended && w.length === 0));
}
const isDestroyed = (s: Node) => !!(s.destroyed || (s._writableState ?? s._readableState)?.destroyed);
function isReadable(s: Node): boolean | null {
  if (typeof s?.readable !== "boolean") return null;
  if (isDestroyed(s)) return false;
  return isReadableNodeStream(s) && s.readable && !isReadableFinished(s);
}
function isWritable(s: Node): boolean | null {
  if (typeof s?.writable !== "boolean") return null;
  if (isDestroyed(s)) return false;
  return isWritableNodeStream(s) && s.writable && !(s.writableEnded === true || s._writableState?.ended);
}
const errored = (s: Node) => s._writableState?.errored ?? s._readableState?.errored ?? null;
function isClosed(s: Node): boolean | null {
  const w = s._writableState, r = s._readableState;
  if (typeof w?.closed === "boolean" || typeof r?.closed === "boolean") return !!(w?.closed || r?.closed);
  return null;
}
function willEmitClose(s: Node): boolean {
  const state = s._writableState ?? s._readableState;
  return !!(state && state.autoDestroy && state.emitClose && state.closed === false);
}

type Options = { readable?: boolean; writable?: boolean; error?: boolean; signal?: AbortSignal };
type Done = (error?: Error | null) => void;

export function finished(stream: unknown, options: Options | Done, callback?: Done): () => void {
  if (typeof options === "function") { callback = options; options = {}; }
  const s = asNode(stream);
  let called = false;
  let done: Done = (error) => { if (called) return; called = true; callback!.call(s, error); };
  const readable = options.readable ?? isReadableNodeStream(s);
  const writable = options.writable ?? isWritableNodeStream(s);
  const w = s._writableState, r = s._readableState;
  let emitsClose = willEmitClose(s) && isReadableNodeStream(s) === readable && isWritableNodeStream(s) === writable;
  let writableFinished = !!isWritableFinished(s, false);
  let readableFinished = !!isReadableFinished(s, false);
  const onfinish = () => {
    writableFinished = true;
    if (s.destroyed) emitsClose = false;
    if (emitsClose && (!s.readable || readable)) return;
    if (!readable || readableFinished) done();
  };
  const onend = () => {
    readableFinished = true;
    if (s.destroyed) emitsClose = false;
    if (emitsClose && (!s.writable || writable)) return;
    if (!writable || writableFinished) done();
  };
  const onerror = (error: Error) => done(error);
  let closed = !!isClosed(s);
  const onclose = () => {
    closed = true;
    const error = errored(s);
    if (error) return done(error);
    if (readable && !readableFinished && isReadableNodeStream(s, true) && !isReadableFinished(s, false)) return done(errors.prematureClose());
    if (writable && !writableFinished && !isWritableFinished(s, false)) return done(errors.prematureClose());
    done();
  };
  const onclosed = () => { closed = true; done(errored(s)); };
  const onlegacyfinish = () => { if (!s.writable) onfinish(); };
  if (writable && !w) { s.on("end", onlegacyfinish); s.on("close", onlegacyfinish); }
  s.on("end", onend);
  s.on("finish", onfinish);
  if (options.error !== false) s.on("error", onerror);
  s.on("close", onclose);
  if (closed) process.nextTick(onclose);
  else if (w?.errorEmitted || r?.errorEmitted) { if (!emitsClose) process.nextTick(onclosed); }
  else if (!readable && (!emitsClose || isReadable(s)) && (writableFinished || isWritable(s) === false)) process.nextTick(onclosed);
  else if (!writable && (!emitsClose || isWritable(s)) && (readableFinished || isReadable(s) === false)) process.nextTick(onclosed);
  const cleanup = () => {
    done = nop;
    s.removeListener("end", onlegacyfinish);
    s.removeListener("close", onlegacyfinish);
    s.removeListener("end", onend);
    s.removeListener("finish", onfinish);
    s.removeListener("error", onerror);
    s.removeListener("close", onclose);
  };
  const { signal } = options;
  if (signal && !closed) {
    const abort = () => { const original = done; cleanup(); original.call(s, errors.aborted(signal)); };
    if (signal.aborted) process.nextTick(abort);
    else {
      signal.addEventListener("abort", abort, { once: true });
      const original = done;
      done = (error) => { signal.removeEventListener("abort", abort); original(error); };
    }
  }
  return cleanup;
}

/** finished as a promise, for stream/promises. */
export function finishedPromise(stream: unknown, options: Options = {}): Promise<void> {
  return new Promise((resolve, reject) => {
    finished(stream, options, (error) => (error ? reject(error) : resolve()));
  });
}
