// The rest of node:events: listener bookkeeping events, prepend, raw
// listeners, limits, events.once/on, errorMonitor and old-style subclasses.
import events, { EventEmitter, errorMonitor, getEventListeners, once, on } from "node:events";
import util from "node:util";
const show = (label, value) => console.log(label, JSON.stringify(value));

const e = new EventEmitter();
const bookkeeping = [];
e.on("newListener", (name) => bookkeeping.push(`new:${String(name)}`));
e.on("removeListener", (name) => bookkeeping.push(`removed:${String(name)}`));
const a = () => {};
e.on("x", a);
e.prependListener("x", () => {});
e.once("x", () => {});
e.prependOnceListener("y", () => {});
show("bookkeeping", bookkeeping);
show("names", e.eventNames().map(String));
show("raw vs unwrapped", { raw: e.rawListeners("x").map((l) => typeof l.listener), unwrapped: e.listeners("x").length, count: e.listenerCount("x"), countOf: e.listenerCount("x", a) });
e.removeAllListeners("x");
show("after removeAll", { bookkeeping: bookkeeping.slice(-3), names: e.eventNames().map(String) });

show("max listeners", [e.getMaxListeners(), e.setMaxListeners(1).getMaxListeners(), EventEmitter.defaultMaxListeners]);
try { e.setMaxListeners(-1); } catch (err) { show("negative max", err.code); }

const order = [];
const target = new EventEmitter();
target.on(errorMonitor, (err) => order.push(`monitor:${err.message}`));
try { target.emit("error", new Error("unhandled")); } catch (err) { order.push(`thrown:${err.message}`); }
try { target.emit("error", "text"); } catch (err) { order.push(`${err.code}:${err.message}`); }
show("errorMonitor", order);

const source = new EventEmitter();
setTimeout(() => source.emit("ready", 1, 2), 1);
show("events.once", await once(source, "ready"));
setTimeout(() => source.emit("error", new Error("failed")), 1);
show("events.once error", await once(source, "ready").catch((err) => err.message));
const ac = new AbortController();
setTimeout(() => ac.abort(), 1);
show("events.once abort", await once(source, "never", { signal: ac.signal }).catch((err) => [err.name, err.code]));

const ticks = new EventEmitter();
setTimeout(() => { ticks.emit("tick", 1); ticks.emit("tick", 2); ticks.emit("tick", 3); }, 1);
const got = [];
for await (const [n] of on(ticks, "tick")) { got.push(n); if (n === 3) break; }
show("events.on", { got, left: ticks.listenerCount("tick") });

function Legacy() { EventEmitter.call(this); }
util.inherits(Legacy, EventEmitter);
const legacy = new Legacy();
let heard = 0;
legacy.on("ping", () => heard++);
legacy.emit("ping");
show("EventEmitter.call", { heard, instance: legacy instanceof EventEmitter, superOf: Legacy.super_ === EventEmitter });
show("module", { isClass: events === EventEmitter, helpers: [typeof events.once, typeof events.on, typeof getEventListeners], listeners: getEventListeners(legacy, "ping").length });
