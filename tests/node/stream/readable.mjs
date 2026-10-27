// Readable: paused and flowing modes, encodings, unshift, destroy and the
// order of its events.
import { Readable } from "node:stream";
const show = (label, value) => console.log(label, JSON.stringify(value));
const tick = () => new Promise((resolve) => setTimeout(resolve, 5));

const log = [];
const paused = new Readable({ read() {} });
paused.on("readable", () => { let chunk; while ((chunk = paused.read()) !== null) log.push(`read:${chunk}`); });
paused.on("end", () => log.push("end"));
paused.on("close", () => log.push("close"));
show("push returns", [paused.push("ab"), paused.push("cd")]);
paused.push(null);
await tick();
show("paused mode", log);

const flowing = [];
const f = new Readable({ read() {} });
f.on("pause", () => flowing.push("pause")).on("resume", () => flowing.push("resume"));
f.on("data", (c) => { flowing.push(`data:${c}`); if (String(c) === "1") { f.pause(); setTimeout(() => f.resume(), 1); } });
f.on("end", () => flowing.push("end"));
f.push("1"); f.push("2"); f.push(null);
await tick(); await tick();
show("flowing mode", { events: flowing, paused: f.isPaused(), flowing: f.readableFlowing, ended: f.readableEnded });

const objects = new Readable({ objectMode: true, read() {} });
objects.push({ a: 1 }); objects.push(2); objects.push(null);
show("object mode", { first: objects.read(), second: objects.read(), length: objects.readableLength, hwm: objects.readableHighWaterMark });

const sized = new Readable({ read() {} });
sized.push("abcdef"); sized.push("gh");
show("read(n)", [String(sized.read(3)), String(sized.read(4)), sized.read(10), sized.readableLength, new Readable({ read() {} }).readableHighWaterMark]);

const text = new Readable({ read() {} });
text.setEncoding("utf8");
const euro = Buffer.from("€");
text.push(euro.subarray(0, 2)); text.push(euro.subarray(2)); text.push(null);
const pieces = [];
text.on("data", (c) => pieces.push([typeof c, c]));
await tick();
show("setEncoding", { pieces, encoding: text.readableEncoding });

const front = new Readable({ read() {} });
front.push("world"); front.unshift("hello ");
show("unshift", String(front.read()));

const order = [];
const broken = new Readable({ read() {} });
broken.on("error", (e) => order.push(`error:${e.message}`)).on("close", () => order.push("close"));
broken.destroy(new Error("boom"));
order.push(`sync:${broken.destroyed}`);
await tick();
show("destroy with error", { order, errored: broken.errored?.message, readable: broken.readable });

const quiet = new Readable({ read() {}, emitClose: false });
const quietEvents = [];
quiet.on("close", () => quietEvents.push("close"));
quiet.destroy();
await tick();
show("emitClose false", quietEvents);

const late = new Readable({ read() {} });
late.push(null);
late.on("error", (e) => show("push after EOF", e.code));
late.push("x");
await tick();
