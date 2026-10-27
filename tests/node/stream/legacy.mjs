// Old-style inheritance with util.inherits and Readable.call(this), and the
// shape of the module itself.
import stream from "node:stream";
import util from "node:util";
import { EventEmitter } from "node:events";
const show = (label, value) => console.log(label, JSON.stringify(value));

function Counter(limit) { stream.Readable.call(this, { objectMode: true }); this.n = 0; this.limit = limit; }
util.inherits(Counter, stream.Readable);
Counter.prototype._read = function () { this.push(this.n < this.limit ? this.n++ : null); };
const seen = [];
for await (const n of new Counter(3)) seen.push(n);
show("inherited readable", { seen, instance: new Counter(1) instanceof stream.Readable, emitter: new Counter(1) instanceof EventEmitter });

class Upper extends stream.Transform { _transform(chunk, enc, cb) { cb(null, String(chunk).toUpperCase()); } }
const upper = new Upper();
const out = [];
upper.on("data", (c) => out.push(String(c)));
upper.end("class");
await new Promise((resolve) => upper.on("end", resolve));
show("class extends", out);

show("module shape", { stream: stream === stream.Stream, keys: ["Readable", "Writable", "Duplex", "Transform", "PassThrough", "pipeline", "finished", "promises"].every((k) => k in stream), hwm: [stream.getDefaultHighWaterMark(false), stream.getDefaultHighWaterMark(true)] });
const legacy = new stream.Stream();
show("legacy stream", { pipe: typeof legacy.pipe, emitter: legacy instanceof EventEmitter });
