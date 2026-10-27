// node:events (RFC 0015). EventEmitter is a plain constructor, as in Node,
// so `EventEmitter.call(this)` with util.inherits keeps working; listeners
// live in `_events` as one function or an array, like Node's.
import type * as nodeEvents from "node:events";
import { inspect } from "ply:runtime";
import { addAbortListener, getEventListeners, on, once } from "./events/helpers";

export type Listener = ((...args: never[]) => unknown) & { listener?: Listener };
type List = Listener[] & { warned?: boolean };
type Events = Record<string | symbol, Listener | List>;
export interface Emitter {
  _events: Events;
  _eventsCount: number;
  _maxListeners: number | undefined;
  emit(type: string | symbol, ...args: unknown[]): boolean;
  listeners(type: string | symbol): Listener[];
}

export const errorMonitor = Symbol("events.errorMonitor");
export const captureRejectionSymbol = Symbol.for("nodejs.rejection");
let defaultMaxListeners = 10;

const invalidListener = (listener: unknown) => Object.assign(
  new TypeError(`The "listener" argument must be of type function. Received ${listener === null ? "null" : typeof listener === "object" ? "an instance of Object" : `type ${typeof listener} (${inspect(listener)})`}`),
  { code: "ERR_INVALID_ARG_TYPE" });
function checkListener(listener: unknown): asserts listener is Listener {
  if (typeof listener !== "function") throw invalidListener(listener);
}
function checkCount(name: string, n: unknown): number {
  if (typeof n !== "number" || n < 0 || Number.isNaN(n)) {
    throw Object.assign(new RangeError(`The value of "${name}" is out of range. It must be a non-negative number. Received ${inspect(n)}`), { code: "ERR_OUT_OF_RANGE" });
  }
  return n;
}

function init(self: Emitter): void {
  if (self._events === undefined || self._events === Object.getPrototypeOf(self)._events) {
    self._events = Object.create(null);
    self._eventsCount = 0;
  }
  self._maxListeners = self._maxListeners || undefined;
}

export function EventEmitter(this: Emitter): void { init(this); }

const listenerCountOf = (self: Emitter, type: string | symbol, listener?: unknown) => {
  const handler = self._events?.[type];
  if (handler === undefined) return 0;
  const all = typeof handler === "function" ? [handler] : handler;
  return listener === undefined ? all.length : all.filter((l) => l === listener || l.listener === listener).length;
};

function addListener(self: Emitter, type: string | symbol, listener: unknown, prepend: boolean): Emitter {
  checkListener(listener);
  if (self._events === undefined) init(self);
  if (self._events.newListener !== undefined) self.emit("newListener", type, listener.listener ?? listener);
  const events = self._events;
  const existing = events[type];
  if (existing === undefined) {
    events[type] = listener;
    self._eventsCount++;
    return self;
  }
  const list: List = typeof existing === "function" ? (events[type] = prepend ? [listener, existing] : [existing, listener]) : existing;
  if (typeof existing !== "function") { if (prepend) list.unshift(listener); else list.push(listener); }
  const max = self._maxListeners ?? defaultMaxListeners;
  if (max > 0 && list.length > max && !list.warned) {
    list.warned = true;
    process.emitWarning(`Possible EventEmitter memory leak detected. ${list.length} ${String(type)} listeners added to ${inspect(self, { depth: -1 })}. MaxListeners is ${max}. Use emitter.setMaxListeners() to increase limit`, "MaxListenersExceededWarning");
  }
  return self;
}

function onceWrapper(self: Emitter, type: string | symbol, listener: Listener): Listener {
  let fired = false;
  const wrapped = function (this: unknown, ...args: unknown[]) {
    if (fired) return undefined;
    fired = true;
    proto.removeListener.call(self, type, wrapped);
    return Reflect.apply(listener, self, args);
  } as Listener;
  wrapped.listener = listener;
  return wrapped;
}

const proto = {
  setMaxListeners(this: Emitter, n: number) { this._maxListeners = checkCount("n", n); return this; },
  getMaxListeners(this: Emitter) { return this._maxListeners ?? defaultMaxListeners; },
  emit(this: Emitter, type: string | symbol, ...args: unknown[]): boolean {
    const events = this._events;
    let unhandled = type === "error";
    if (events !== undefined) {
      if (unhandled && events[errorMonitor] !== undefined) this.emit(errorMonitor, ...args);
      unhandled = unhandled && events.error === undefined;
    } else if (!unhandled) return false;
    if (unhandled) {
      const error = args[0];
      if (error instanceof Error) throw error;
      throw Object.assign(new Error(`Unhandled error. (${inspect(error)})`), { code: "ERR_UNHANDLED_ERROR", context: error });
    }
    const handler = events[type];
    if (handler === undefined) return false;
    if (typeof handler === "function") Reflect.apply(handler, this, args);
    else for (const listener of [...handler]) Reflect.apply(listener, this, args);
    return true;
  },
  on(this: Emitter, type: string | symbol, listener: Listener) { return addListener(this, type, listener, false); },
  addListener(this: Emitter, type: string | symbol, listener: Listener) { return addListener(this, type, listener, false); },
  prependListener(this: Emitter, type: string | symbol, listener: Listener) { return addListener(this, type, listener, true); },
  once(this: Emitter, type: string | symbol, listener: Listener) {
    checkListener(listener);
    return addListener(this, type, onceWrapper(this, type, listener), false);
  },
  prependOnceListener(this: Emitter, type: string | symbol, listener: Listener) {
    checkListener(listener);
    return addListener(this, type, onceWrapper(this, type, listener), true);
  },
  removeListener(this: Emitter, type: string | symbol, listener: Listener) {
    checkListener(listener);
    const events = this._events;
    const list = events?.[type];
    if (list === undefined) return this;
    if (list === listener || (typeof list === "function" && list.listener === listener)) {
      if (--this._eventsCount === 0) this._events = Object.create(null);
      else delete events[type];
      if (this._events.removeListener !== undefined) this.emit("removeListener", type, list.listener ?? listener);
    } else if (typeof list !== "function") {
      let index = -1;
      for (let i = list.length - 1; i >= 0; i--) if (list[i] === listener || list[i].listener === listener) { index = i; break; }
      if (index < 0) return this;
      const [removed] = list.splice(index, 1);
      if (list.length === 1) events[type] = list[0];
      if (events.removeListener !== undefined) this.emit("removeListener", type, removed.listener ?? listener);
    }
    return this;
  },
  off(this: Emitter, type: string | symbol, listener: Listener) { return proto.removeListener.call(this, type, listener); },
  removeAllListeners(this: Emitter, type?: string | symbol) {
    const events = this._events;
    if (events === undefined) return this;
    if (events.removeListener === undefined) {
      if (type === undefined) { this._events = Object.create(null); this._eventsCount = 0; }
      else if (events[type] !== undefined) { if (--this._eventsCount === 0) this._events = Object.create(null); else delete events[type]; }
      return this;
    }
    // Each removal is announced, the removeListener listeners last.
    if (type === undefined) {
      for (const key of Reflect.ownKeys(events)) if (key !== "removeListener") proto.removeAllListeners.call(this, key);
      proto.removeAllListeners.call(this, "removeListener");
      this._events = Object.create(null);
      this._eventsCount = 0;
      return this;
    }
    const listeners = events[type];
    if (typeof listeners === "function") proto.removeListener.call(this, type, listeners);
    else if (listeners !== undefined) for (let i = listeners.length - 1; i >= 0; i--) proto.removeListener.call(this, type, listeners[i]);
    return this;
  },
  listeners(this: Emitter, type: string | symbol) { return proto.rawListeners.call(this, type).map((l) => l.listener ?? l); },
  rawListeners(this: Emitter, type: string | symbol): Listener[] {
    const handler = this._events?.[type];
    return handler === undefined ? [] : typeof handler === "function" ? [handler] : [...handler];
  },
  listenerCount(this: Emitter, type: string | symbol, listener?: unknown) { return listenerCountOf(this, type, listener); },
  eventNames(this: Emitter) { return this._eventsCount > 0 ? Reflect.ownKeys(this._events) : []; },
};
Object.assign(EventEmitter.prototype, proto, { _events: undefined, _eventsCount: 0, _maxListeners: undefined });

Object.defineProperty(EventEmitter, "defaultMaxListeners", {
  enumerable: true,
  get: () => defaultMaxListeners,
  set: (n: unknown) => { defaultMaxListeners = checkCount("defaultMaxListeners", n); },
});

export function setMaxListeners(n = defaultMaxListeners, ...targets: Emitter[]): void {
  checkCount("n", n);
  if (targets.length === 0) defaultMaxListeners = n;
  else for (const target of targets) proto.setMaxListeners.call(target, n);
}
export function getMaxListeners(target: Emitter): number { return proto.getMaxListeners.call(target); }
export function listenerCount(emitter: Emitter, type: string | symbol): number { return listenerCountOf(emitter, type); }
export const captureRejections = false;
export const usingDomains = false;
export { addAbortListener, getEventListeners, on, once };

// The module itself is EventEmitter with these on it, as `require("events")`.
Object.assign(EventEmitter, {
  EventEmitter, errorMonitor, captureRejectionSymbol, captureRejections, usingDomains, init: function (this: Emitter) { init(this); },
  once, on, getEventListeners, getMaxListeners, setMaxListeners, listenerCount, addAbortListener,
});
export default EventEmitter as unknown as typeof nodeEvents.EventEmitter;
