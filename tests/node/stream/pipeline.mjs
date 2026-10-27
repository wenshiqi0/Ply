// pipeline, finished, stream/promises and addAbortSignal.
import { PassThrough, Readable, Transform, Writable, addAbortSignal, finished, pipeline } from "node:stream";
import * as promises from "node:stream/promises";
const show = (label, value) => console.log(label, JSON.stringify(value));
const tick = () => new Promise((resolve) => setTimeout(resolve, 5));
const sink = (into) => new Writable({ objectMode: true, write(c, e, cb) { into.push(String(c)); cb(); } });

const collected = [];
await new Promise((resolve) => pipeline(Readable.from(["a", "b"]), new PassThrough({ objectMode: true }), sink(collected), (err) => { show("pipeline ok", { err: err ?? null, collected }); resolve(); }));

const states = {};
const source = Readable.from(["x"]);
const failing = new Transform({ transform(c, e, cb) { cb(new Error("bad transform")); } });
const target = sink([]);
await new Promise((resolve) => pipeline(source, failing, target, (err) => { Object.assign(states, { message: err.message, source: source.destroyed, target: target.destroyed }); resolve(); }));
show("pipeline failure", states);

const early = new PassThrough();
const earlyResult = new Promise((resolve) => finished(early, (err) => resolve(err?.code ?? "ok")));
early.destroy();
show("premature close", await earlyResult);

const done = new PassThrough();
const doneResult = new Promise((resolve) => finished(done, (err) => resolve(err ?? "ok")));
done.end("x"); done.resume();
show("finished", await doneResult);

const words = [];
await promises.pipeline(Readable.from(["one", "two"]), async function* (src) { for await (const chunk of src) yield chunk.length; }, sink(words));
show("generator stage", words);
show("promise value", await promises.pipeline(Readable.from([1, 2, 3]), async (src) => { let sum = 0; for await (const n of src) sum += n; return sum; }));
await promises.finished(Readable.from([]).resume());
show("promises.finished", "resolved");

const controller = new AbortController();
const aborted = addAbortSignal(controller.signal, new PassThrough());
const abortInfo = new Promise((resolve) => aborted.on("error", (e) => resolve({ name: e.name, code: e.code })));
controller.abort();
show("addAbortSignal", await abortInfo);
await tick();
