// AbortController, AbortSignal, EventTarget and DOMException.
const show = (label, value) => console.log(label, JSON.stringify(value));
const c = new AbortController();
const seen = [];
c.signal.addEventListener("abort", (event) => seen.push(["listener", event.type, event.target === c.signal]));
c.signal.onabort = () => seen.push(["onabort"]);
show("before", { aborted: c.signal.aborted, reason: c.signal.reason ?? null });
c.abort();
c.abort("again");
show("after", { aborted: c.signal.aborted, name: c.signal.reason.name, message: c.signal.reason.message, code: c.signal.reason.code, isError: c.signal.reason instanceof Error, isDOMException: c.signal.reason instanceof DOMException, seen });
try { c.signal.throwIfAborted(); } catch (e) { show("throwIfAborted", e.name); }
const custom = new AbortController();
custom.abort(42);
show("custom reason", custom.signal.reason);
show("AbortSignal.abort", [AbortSignal.abort().aborted, AbortSignal.abort("why").reason]);
const a = new AbortController(), b = new AbortController();
const any = AbortSignal.any([a.signal, b.signal]);
b.abort("b first");
show("any", [any.aborted, any.reason]);
// Node does not keep the process alive for AbortSignal.timeout alone.
const timeout = AbortSignal.timeout(5);
const alive = setTimeout(() => {}, 1000);
await new Promise((resolve) => timeout.addEventListener("abort", resolve, { once: true }));
clearTimeout(alive);
show("timeout", { name: timeout.reason.name, code: timeout.reason.code });
try { new AbortSignal(); } catch (e) { show("constructor", e.name); }
const t = new EventTarget();
let count = 0;
const listener = () => count++;
t.addEventListener("x", listener);
t.addEventListener("x", listener);
t.addEventListener("x", () => count += 10, { once: true });
t.dispatchEvent(new Event("x"));
t.dispatchEvent(new Event("x"));
t.removeEventListener("x", listener);
t.dispatchEvent(new Event("x"));
show("EventTarget", count);
const e = new Event("y", { cancelable: true });
e.preventDefault();
show("Event", { type: e.type, defaultPrevented: e.defaultPrevented, dispatched: new EventTarget().dispatchEvent(e) });
show("DOMException", [new DOMException("m", "NotSupportedError").code, new DOMException("m").name, new DOMException().message]);
