// Readable as an async iterable, and Readable.from(iterable).
import { Readable } from "./readable";
import type { ReadableImpl, ReadableOptions } from "./readable";
import { destroyer } from "./destroy";
import { finished } from "./finished";
import { errors } from "./state";

/** Reads chunk by chunk, waiting for 'readable'; destroys on early exit unless asked not to. */
async function* iterate(stream: ReadableImpl, options?: { destroyOnReturn?: boolean }): AsyncGenerator<unknown> {
  let wake: () => void = () => {};
  const next = function (this: unknown, resolve?: () => void) {
    if (this === stream) { wake(); wake = () => {}; } else if (resolve) wake = resolve;
  };
  stream.on("readable", next);
  let error: Error | null | undefined;
  const cleanup = finished(stream, { writable: false }, (err?: Error | null) => {
    error = err ? err : null;
    wake();
    wake = () => {};
  });
  try {
    for (;;) {
      const chunk = stream.destroyed ? null : stream.read();
      if (chunk !== null) yield chunk;
      else if (error) throw error;
      else if (error === null) return;
      else await new Promise<void>((resolve) => next.call(undefined, resolve));
    }
  } catch (thrown) {
    error = error ?? (thrown as Error);
    throw error;
  } finally {
    if ((error || options?.destroyOnReturn !== false) && (error === undefined || stream._readableState.autoDestroy)) destroyer(stream, null);
    else { stream.removeListener("readable", next); cleanup(); }
  }
}

Object.assign(Readable.prototype, {
  [Symbol.asyncIterator](this: ReadableImpl) { return iterate(this); },
  iterator(this: ReadableImpl, options?: { destroyOnReturn?: boolean }) { return iterate(this, options); },
});

type Source = Iterable<unknown> | AsyncIterable<unknown> | string | Buffer;
/** A Readable in object mode over the values of an iterable or async iterable. */
export function from(iterable: Source, options?: ReadableOptions): ReadableImpl {
  const make = (opts: ReadableOptions) => new (Readable as unknown as new (o: ReadableOptions) => ReadableImpl)(opts);
  if (typeof iterable === "string" || iterable instanceof Buffer) {
    return make({ objectMode: true, ...options, read() { this.push(iterable); this.push(null); } });
  }
  let iterator: Iterator<unknown> | AsyncIterator<unknown>;
  let isAsync: boolean;
  if (iterable && typeof (iterable as AsyncIterable<unknown>)[Symbol.asyncIterator] === "function") {
    isAsync = true;
    iterator = (iterable as AsyncIterable<unknown>)[Symbol.asyncIterator]();
  } else if (iterable && typeof (iterable as Iterable<unknown>)[Symbol.iterator] === "function") {
    isAsync = false;
    iterator = (iterable as Iterable<unknown>)[Symbol.iterator]();
  } else {
    throw Object.assign(new TypeError(`The "iterable" argument must be an instance of Iterable. Received ${iterable === null ? "null" : typeof iterable}`), { code: "ERR_INVALID_ARG_TYPE" });
  }
  const readable = make({ objectMode: true, highWaterMark: 1, ...options });
  let reading = false;
  async function close(error: Error | null) {
    if (error && typeof iterator.throw === "function") {
      const { value, done } = await iterator.throw(error);
      await value;
      if (done) return;
    }
    if (typeof iterator.return === "function") { const { value } = await iterator.return(); await value; }
  }
  async function pull() {
    for (;;) {
      try {
        const result = isAsync ? await iterator.next() : iterator.next();
        const { value, done } = result instanceof Promise ? await result : result;
        if (done) { readable.push(null); return; }
        const resolved = !isAsync && value && typeof (value as PromiseLike<unknown>).then === "function" ? await value : value;
        if (resolved === null) { reading = false; throw errors.nullValues(); }
        if (readable.push(resolved)) continue;
        reading = false;
      } catch (error) {
        readable.destroy(error as Error);
      }
      break;
    }
  }
  readable._read = () => { if (!reading) { reading = true; void pull(); } };
  readable._destroy = (error, callback) => {
    close(error).then(() => process.nextTick(callback, error), (e: Error) => process.nextTick(callback, e || error));
  };
  return readable;
}
Object.assign(Readable, { from });
