// node:events' module-level helpers. They work on any emitter through its
// public methods, and on EventTargets such as AbortSignal.
import type * as nodeEvents from "node:events";

type Listener = (...args: never[]) => unknown;
interface EmitterLike {
  on(type: string | symbol, listener: Listener): unknown;
  once(type: string | symbol, listener: Listener): unknown;
  removeListener(type: string | symbol, listener: Listener): unknown;
  listeners?(type: string | symbol): Listener[];
}
type Source = EmitterLike | EventTarget;
const isTarget = (source: Source): source is EventTarget => typeof (source as EventTarget).addEventListener === "function" && typeof (source as EmitterLike).on !== "function";

/** Node's AbortError for helpers stopped by a signal. */
export const abortError = (signal?: AbortSignal) => Object.assign(new Error("The operation was aborted", { cause: signal?.reason }), { name: "AbortError", code: "ABORT_ERR" });

/** Resolves with the arguments of the next `name` event, rejects on "error". */
export function once(source: Source, name: string | symbol, options: { signal?: AbortSignal } = {}): Promise<unknown[]> {
  const { signal } = options;
  if (signal?.aborted) return Promise.reject(abortError(signal));
  return new Promise((resolve, reject) => {
    const cleanups: (() => void)[] = [];
    const settle = (fn: () => void) => { for (const cleanup of cleanups) cleanup(); fn(); };
    if (isTarget(source)) {
      const listener = (...args: unknown[]) => settle(() => resolve(args));
      source.addEventListener(String(name), listener, { once: true });
      cleanups.push(() => source.removeEventListener(String(name), listener));
    } else {
      const resolver = (...args: unknown[]) => settle(() => resolve(args));
      source.once(name, resolver);
      cleanups.push(() => source.removeListener(name, resolver));
      if (name !== "error") {
        const errorListener = (error: unknown) => settle(() => reject(error));
        source.once("error", errorListener);
        cleanups.push(() => source.removeListener("error", errorListener));
      }
    }
    if (signal) {
      const abort = () => settle(() => reject(abortError(signal)));
      signal.addEventListener("abort", abort, { once: true });
      cleanups.push(() => signal.removeEventListener("abort", abort));
    }
  });
}

/** An async iterator over every `name` event's arguments, until "error" or return(). */
export function on(source: Source, name: string | symbol, options: { signal?: AbortSignal; close?: readonly (string | symbol)[] } = {}): AsyncIterableIterator<unknown[]> {
  const { signal } = options;
  if (signal?.aborted) throw abortError(signal);
  const queue: unknown[][] = [];
  const waiting: { resolve: (r: IteratorResult<unknown[]>) => void; reject: (e: unknown) => void }[] = [];
  let failure: { error: unknown } | null = null;
  let finished = false;
  const removers: (() => void)[] = [];
  const listen = (type: string | symbol, listener: (...args: unknown[]) => void) => {
    if (isTarget(source)) {
      source.addEventListener(String(type), listener);
      removers.push(() => source.removeEventListener(String(type), listener));
    } else {
      source.on(type, listener);
      removers.push(() => source.removeListener(type, listener));
    }
  };
  const finish = () => {
    finished = true;
    for (const remove of removers.splice(0)) remove();
    for (const w of waiting.splice(0)) w.resolve({ value: undefined, done: true });
  };
  const fail = (error: unknown) => {
    const waiter = waiting.shift();
    if (waiter) waiter.reject(error); else failure = { error };
    finish();
  };
  listen(name, (...args) => {
    const waiter = waiting.shift();
    if (waiter) waiter.resolve({ value: args, done: false }); else queue.push(args);
  });
  if (name !== "error" && !isTarget(source)) listen("error", fail);
  for (const close of options.close ?? []) listen(close, finish);
  if (signal) {
    const abort = () => fail(abortError(signal));
    signal.addEventListener("abort", abort, { once: true });
    removers.push(() => signal.removeEventListener("abort", abort));
  }
  return {
    next(): Promise<IteratorResult<unknown[]>> {
      if (queue.length) return Promise.resolve({ value: queue.shift()!, done: false });
      if (failure) { const { error } = failure; failure = null; return Promise.reject(error); }
      if (finished) return Promise.resolve({ value: undefined, done: true });
      return new Promise((resolve, reject) => waiting.push({ resolve, reject }));
    },
    return(): Promise<IteratorResult<unknown[]>> { finish(); return Promise.resolve({ value: undefined, done: true }); },
    throw(error: unknown): Promise<IteratorResult<unknown[]>> { finish(); return Promise.reject(error); },
    [Symbol.asyncIterator]() { return this; },
  };
}

/** The listeners of an emitter; an EventTarget's are private to it. */
export function getEventListeners(source: Source, name: string | symbol): Listener[] {
  return isTarget(source) ? [] : source.listeners?.(name) ?? [];
}

/** Calls `listener` once when `signal` aborts; disposing the result stops that. */
export const addAbortListener: typeof nodeEvents.addAbortListener = (signal, listener) => {
  if (signal.aborted) queueMicrotask(() => listener(new Event("abort")));
  else signal.addEventListener("abort", listener, { once: true });
  return { [Symbol.dispose]: () => signal.removeEventListener("abort", listener) };
};
