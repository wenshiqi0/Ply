// stream.pipeline: streams, iterables and async generator functions joined
// in order; any failure destroys every stage and reaches the callback once.
import { destroyer } from "./destroy";
import { finished, isNodeStream, isReadableFinished, isReadableNodeStream } from "./finished";
import { errors } from "./state";
import type { Base, Writable } from "./state";

type Stage = unknown;
type Done = (error?: Error | null, value?: unknown) => void;
const isIterable = (value: unknown) => !!value && typeof value === "object" &&
  (typeof (value as AsyncIterable<unknown>)[Symbol.asyncIterator] === "function" || typeof (value as Iterable<unknown>)[Symbol.iterator] === "function");

function pipeStreams(src: Base & { pipe(dst: unknown, options: object): unknown }, dst: Writable, finish: Done, end: boolean): void {
  let ended = false;
  dst.on("close", () => { if (!ended) finish(errors.prematureClose()); });
  src.pipe(dst, { end: false });
  if (end) {
    const endFn = () => { ended = true; dst.end(); };
    if (isReadableFinished(src)) process.nextTick(endFn); else src.once("end", endFn);
    finished(src, { readable: true, writable: false }, (error) => {
      const r = src._readableState;
      if (error && (error as { code?: string }).code === "ERR_STREAM_PREMATURE_CLOSE" && r?.ended && !r.errored && !r.errorEmitted) { src.once("end", finish); src.once("error", finish); }
      else finish(error);
    });
  } else finish();
  finished(dst, { readable: false, writable: true }, finish);
}

async function pump(iterable: AsyncIterable<unknown> | Iterable<unknown>, dst: Writable, finish: Done, end: boolean): Promise<void> {
  try {
    for await (const chunk of iterable as AsyncIterable<unknown>) {
      if (!dst.write(chunk)) {
        if (dst.destroyed) return;
        await new Promise<void>((resolve, reject) => {
          const drain = () => { dst.removeListener("error", fail); resolve(); };
          const fail = (error: Error) => { dst.removeListener("drain", drain); reject(error); };
          dst.once("drain", drain);
          dst.once("error", fail);
        });
      }
    }
    if (end) dst.end();
    await new Promise<void>((resolve, reject) => finished(dst, { readable: false }, (error) => (error ? reject(error) : resolve())));
    finish();
  } catch (error) {
    finish(error as Error);
  }
}

export function pipeline(...args: unknown[]): unknown {
  const callback = args.pop();
  if (typeof callback !== "function") throw Object.assign(new TypeError(`The "streams[stream.length - 1]" property must be of type function. Received ${typeof callback}`), { code: "ERR_INVALID_ARG_TYPE" });
  const streams: Stage[] = args.length === 1 && Array.isArray(args[0]) ? args[0] : args;
  if (streams.length < 2) throw Object.assign(new TypeError('The "streams" argument must be specified'), { code: "ERR_MISSING_ARGS" });
  const controller = new AbortController();
  const destroys: ((error?: Error | null) => void)[] = [];
  let error: Error | undefined;
  let value: unknown;
  let pending = 0;
  let called = false;
  const finishWith = (err: Error | null | undefined, final: boolean) => {
    if (err && (!error || (error as { code?: string }).code === "ERR_STREAM_PREMATURE_CLOSE")) error = err;
    if (!error && !final) return;
    while (destroys.length) destroys.shift()!(error);
    controller.abort();
    if (final && !called) { called = true; process.nextTick(callback as Done, error, value); }
  };
  const finish: Done = (err) => finishWith(err, --pending === 0);
  let ret: unknown;
  streams.forEach((stage, i) => {
    const reading = i < streams.length - 1;
    const writing = i > 0;
    if (isNodeStream(stage)) {
      let done = false;
      (stage as Base).on("close", () => { done = true; });
      finished(stage, { readable: reading, writable: writing }, (err) => { done = !err; });
      destroys.push((err) => { if (!done) { done = true; destroyer(stage, err || errors.destroyed("pipe")); } });
    }
    if (i === 0) {
      ret = typeof stage === "function" ? stage({ signal: controller.signal }) : stage;
      if (typeof stage === "function" && !isIterable(ret)) throw Object.assign(new TypeError("Expected Iterable, AsyncIterable or Stream to be returned from the \"streams[0]\" function."), { code: "ERR_INVALID_RETURN_VALUE" });
    } else if (typeof stage === "function") {
      const source = ret;
      ret = stage(source, { signal: controller.signal });
      if (!reading) {
        pending++;
        Promise.resolve(ret).then((result) => { value = result; finish(); }, (err: Error) => finish(err));
      }
    } else if (isNodeStream(stage)) {
      if (isReadableNodeStream(ret)) { pending += 2; pipeStreams(ret as Base & { pipe(d: unknown, o: object): unknown }, stage as Writable, finish, true); }
      else if (isIterable(ret)) { pending++; void pump(ret as AsyncIterable<unknown>, stage as Writable, finish, true); }
      else throw Object.assign(new TypeError("The \"val\" argument must be a stream or an iterable."), { code: "ERR_INVALID_ARG_TYPE" });
      ret = stage;
    } else throw Object.assign(new TypeError(`The "streams[${i}]" argument must be a stream or function.`), { code: "ERR_INVALID_ARG_TYPE" });
  });
  if (pending === 0) process.nextTick(() => finishWith(undefined, true));
  return ret;
}

/** pipeline as a promise, for stream/promises. */
export function pipelinePromise(...streams: unknown[]): Promise<unknown> {
  return new Promise((resolve, reject) => {
    pipeline(...streams, (error: Error | null | undefined, value: unknown) => (error ? reject(error) : resolve(value)));
  });
}
