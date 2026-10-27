// node:util (RFC 0015). format and inspect are the prelude's, which
// console.log shares.
import type * as nodeUtil from "node:util";
import { deepEqual, format, inspect } from "ply:runtime";
import * as types from "./util/types";
import { colors, stripVTControlCharacters, styleText } from "./util/style";

type Callback = (error: unknown, value?: unknown) => void;
type Fn = (...args: never[]) => unknown;

Object.assign(inspect, { custom: Symbol.for("nodejs.util.inspect.custom"), colors });
const custom = Symbol.for("nodejs.util.promisify.custom");

/** A callback-last function as a function returning a promise. */
export function promisify(fn: Fn & { [custom]?: Fn }) {
  if (typeof fn !== "function") throw Object.assign(new TypeError(`The "original" argument must be of type function`), { code: "ERR_INVALID_ARG_TYPE" });
  if (typeof fn[custom] === "function") return fn[custom];
  const promised = function (this: unknown, ...args: unknown[]) {
    return new Promise((resolve, reject) => Reflect.apply(fn, this, [...args, (error: unknown, value: unknown) => (error ? reject(error) : resolve(value))]));
  };
  Object.setPrototypeOf(promised, Object.getPrototypeOf(fn));
  Object.defineProperty(promised, custom, { value: promised, configurable: true });
  return Object.defineProperties(promised, Object.getOwnPropertyDescriptors(fn));
}
promisify.custom = custom;

/** A function returning a promise as a callback-last function. */
export function callbackify(fn: (...args: never[]) => Promise<unknown>) {
  if (typeof fn !== "function") throw Object.assign(new TypeError(`The "original" argument must be of type function`), { code: "ERR_INVALID_ARG_TYPE" });
  return function (this: unknown, ...args: unknown[]) {
    const callback = args.pop() as Callback;
    if (typeof callback !== "function") throw Object.assign(new TypeError(`The last argument must be of type function`), { code: "ERR_INVALID_ARG_TYPE" });
    Reflect.apply(fn, this, args).then(
      (value: unknown) => process.nextTick(callback, null, value),
      (reason: unknown) => process.nextTick(callback, reason || Object.assign(new Error("Promise was rejected with falsy value"), { code: "ERR_FALSY_VALUE_REJECTION", reason })),
    );
  };
}

/** Prototype inheritance the way Node 0.x libraries set it up. */
export const inherits: typeof nodeUtil.inherits = (ctor, superCtor) => {
  if (typeof ctor !== "function" || typeof superCtor !== "function" || superCtor.prototype === undefined) {
    throw Object.assign(new TypeError('The "ctor" and "superCtor" arguments must be functions with a prototype'), { code: "ERR_INVALID_ARG_TYPE" });
  }
  Object.defineProperty(ctor, "super_", { value: superCtor, writable: true, configurable: true });
  Object.setPrototypeOf(ctor.prototype, superCtor.prototype);
};

/** Wraps `fn` so its first use warns that it is deprecated. */
export function deprecate<T extends Fn>(fn: T, message: string, code?: string): T {
  let warned = false;
  return function (this: unknown, ...args: unknown[]) {
    if (!warned) { warned = true; process.emitWarning(message, { type: "DeprecationWarning", code }); }
    return new.target ? Reflect.construct(fn, args, new.target) : Reflect.apply(fn, this, args);
  } as unknown as T;
}

/** A logger for `section` that writes to stderr only when NODE_DEBUG names it. */
export function debuglog(section: string, callback?: (fn: (...args: unknown[]) => void) => void) {
  const pattern = (process.env.NODE_DEBUG ?? "").split(/[\s,]+/).filter(Boolean)
    .map((part) => new RegExp(`^${part.replace(/[|\\{}()[\]^$+?.]/g, "\\$&").replace(/\*/g, ".*")}$`, "i"));
  const enabled = pattern.some((re) => re.test(section));
  const log = Object.assign(enabled
    ? (...args: unknown[]) => { process.stderr.write(`${section.toUpperCase()} ${process.pid}: ${format(...args)}\n`); }
    : () => {}, { enabled });
  callback?.(log);
  return log;
}

/** Resolves when `signal` aborts. */
export function aborted(signal: AbortSignal, _resource: object): Promise<void> {
  return new Promise((resolve) => (signal.aborted ? resolve() : signal.addEventListener("abort", () => resolve(), { once: true })));
}

export const isDeepStrictEqual: typeof nodeUtil.isDeepStrictEqual = (a, b) => deepEqual(a, b, new Map());
export const formatWithOptions = (_options: object, ...args: unknown[]): string => format(...args);
export const toUSVString = (text: string): string => String(text).replace(/[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/g, "�");
export const isArray = Array.isArray;
export const debug = debuglog;
export { format, inspect, types, styleText, stripVTControlCharacters };
export const TextEncoder = globalThis.TextEncoder;
export const TextDecoder = globalThis.TextDecoder;
