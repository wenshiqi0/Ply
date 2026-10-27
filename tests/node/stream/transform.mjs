// Transform, PassThrough and Duplex.
import { Duplex, PassThrough, Transform, Writable, Readable } from "node:stream";
const show = (label, value) => console.log(label, JSON.stringify(value));
const tick = () => new Promise((resolve) => setTimeout(resolve, 5));

const out = [];
const upper = new Transform({
  transform(chunk, enc, cb) { cb(null, String(chunk).toUpperCase()); },
  flush(cb) { this.push("!"); cb(); },
});
upper.on("data", (c) => out.push(String(c)));
upper.on("end", () => out.push("end"));
upper.write("a"); upper.write("b"); upper.end("c");
await tick();
show("transform", out);

const split = new Transform({ readableObjectMode: true, transform(chunk, enc, cb) { for (const part of String(chunk).split(",")) this.push(part); cb(); } });
const parts = [];
split.on("data", (p) => parts.push(p));
split.end("x,y,z");
await tick();
show("one to many", parts);

const pt = new PassThrough();
const through = [];
pt.on("data", (c) => through.push(String(c)));
pt.write("p"); pt.end("q");
await tick();
show("passthrough", through);

const halfEvents = [];
const half = new Duplex({ allowHalfOpen: false, read() {}, write(c, e, cb) { cb(); } });
half.on("finish", () => halfEvents.push("finish")).on("end", () => halfEvents.push("end")).on("close", () => halfEvents.push("close"));
half.resume();
half.push(null);
await tick();
show("allowHalfOpen false", { events: halfEvents, writableEnded: half.writableEnded });

const piped = [];
Readable.from(["1", "2", "3"]).pipe(new Transform({ transform(c, e, cb) { cb(null, `<${c}>`); } }))
  .pipe(new Writable({ write(c, e, cb) { piped.push(String(c)); cb(); } })).on("finish", () => piped.push("finish"));
await tick();
show("pipe chain", piped);
show("instanceof", { duplexIsWritable: new Duplex() instanceof Writable, transformIsDuplex: new PassThrough() instanceof Duplex, readableIsWritable: new Readable() instanceof Writable });
