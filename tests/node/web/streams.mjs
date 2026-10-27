// ReadableStream readers, pulling, tee, async iteration and cancellation.
const show = (label, value) => console.log(label, JSON.stringify(value));
const s = new ReadableStream({ start(c) { c.enqueue("a"); c.enqueue("b"); c.close(); } });
const reader = s.getReader();
show("locked", s.locked);
show("reads", [await reader.read(), await reader.read(), await reader.read(), await reader.read()]);
reader.releaseLock();
show("released", s.locked);
let n = 0;
const pulled = new ReadableStream({ pull(c) { n++; if (n > 3) c.close(); else c.enqueue(n); } }, { highWaterMark: 0 });
const out = [];
for await (const chunk of pulled) out.push(chunk);
show("pull and iterate", out);
const [left, right] = new ReadableStream({ start(c) { c.enqueue(1); c.enqueue(2); c.close(); } }).tee();
const collect = async (stream) => { const all = []; for await (const x of stream) all.push(x); return all; };
show("tee", [await collect(left), await collect(right)]);
let cancelled;
const cancellable = new ReadableStream({ pull(c) { c.enqueue("x"); }, cancel(reason) { cancelled = reason; } });
const r2 = cancellable.getReader();
await r2.read();
await r2.cancel("stop");
show("cancel", { cancelled, after: await r2.read() });
const failing = new ReadableStream({ start(c) { c.error(new TypeError("broken")); } });
show("error", await failing.getReader().read().then(() => "read", (e) => `${e.name} ${e.message}`));
for await (const chunk of new ReadableStream({ start(c) { c.enqueue("first"); c.enqueue("second"); } })) { show("break", chunk); break; }
try { s.getReader(); s.getReader(); } catch (e) { show("double lock", e.name); }
