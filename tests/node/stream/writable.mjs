// Writable: backpressure and drain, corking into _writev, end and the
// order of prefinish, finish and close.
import { Writable } from "node:stream";
const show = (label, value) => console.log(label, JSON.stringify(value));
const tick = () => new Promise((resolve) => setTimeout(resolve, 5));

const log = [];
const w = new Writable({ highWaterMark: 3, write(chunk, enc, cb) { log.push(`write:${chunk}:${enc}`); setTimeout(cb, 1); } });
w.on("drain", () => log.push("drain"));
show("write returns", [w.write("ab", () => log.push("cb:ab")), w.write("c", () => log.push("cb:c")), w.writableLength, w.writableNeedDrain]);
await tick(); await tick();
show("drain order", log);

const events = [];
const ender = new Writable({
  write(chunk, enc, cb) { events.push(`write:${chunk}`); cb(); },
  final(cb) { events.push("final"); cb(); },
});
for (const name of ["prefinish", "finish", "close"]) ender.on(name, () => events.push(name));
ender.end("last", () => events.push("end callback"));
events.push(`sync ended:${ender.writableEnded} finished:${ender.writableFinished}`);
await tick();
show("end order", { events, finished: ender.writableFinished, writable: ender.writable });

const batches = [];
const corked = new Writable({
  write(chunk, enc, cb) { batches.push(["write", String(chunk)]); cb(); },
  writev(chunks, cb) { batches.push(["writev", chunks.map((c) => String(c.chunk))]); cb(); },
});
corked.cork();
corked.write("a"); corked.write("b"); corked.write("c");
show("corked", corked.writableCorked);
process.nextTick(() => corked.uncork());
await tick();
show("writev", batches);

const after = new Writable({ write(c, e, cb) { cb(); } });
after.end();
after.on("error", (e) => show("write after end", { code: e.code, message: e.message }));
after.write("x", (e) => show("write callback", e?.code));
await tick();

const objects = [];
const om = new Writable({ objectMode: true, write(chunk, enc, cb) { objects.push(chunk); cb(); } });
om.write({ n: 1 }); om.write(2); om.end();
await tick();
show("object mode", objects);

const raw = [];
const keep = new Writable({ decodeStrings: false, write(chunk, enc, cb) { raw.push([typeof chunk, enc]); cb(); } });
keep.write("text", "latin1");
keep.write(Buffer.from("x"));
await tick();
show("decodeStrings false", raw);

const failing = new Writable({ write(c, e, cb) { cb(new Error("nope")); } });
const failures = [];
failing.on("error", (e) => failures.push(`error:${e.message}`)).on("close", () => failures.push("close"));
failing.write("x", (e) => failures.push(`cb:${e?.message}`));
await tick();
show("write error", { failures, destroyed: failing.destroyed });
try { new Writable().write(null); } catch (e) { show("null chunk", e.code); }
