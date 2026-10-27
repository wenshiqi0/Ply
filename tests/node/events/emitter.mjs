// EventEmitter listener order, once, removal and the error event.
import { EventEmitter } from "node:events";
import events from "node:events";
const show = (label, value) => console.log(label, JSON.stringify(value));
show("default export", events === EventEmitter || events.EventEmitter === EventEmitter);
const e = new EventEmitter();
const calls = [];
const first = (x) => calls.push(["first", x]);
e.on("tick", first);
e.once("tick", (x) => calls.push(["once", x]));
e.addListener("tick", function (x) { calls.push(["third", x, this === e]); });
show("emit", [e.emit("tick", 1), e.emit("tick", 2), e.emit("none")]);
show("calls", calls);
show("counts", { tick: e.listenerCount("tick"), listeners: e.listeners("tick").length });
e.off("tick", first);
e.removeListener("tick", first);
show("after off", e.listenerCount("tick"));
e.removeAllListeners("tick");
show("after removeAll", e.listenerCount("tick"));
try { e.emit("error", new Error("boom")); show("error", "ignored"); } catch (err) { show("error thrown", err.message); }
e.on("error", (err) => show("error handled", err.message));
show("emit error", e.emit("error", new Error("caught")));
show("chaining", e.on("x", () => {}) === e);
