// Request and Response bodies: reading once, cloning and constructors.
const show = (label, value) => console.log(label, JSON.stringify(value));
const r = new Response("hello", { status: 201, statusText: "Created", headers: { "x-a": "1" } });
show("response", { status: r.status, statusText: r.statusText, ok: r.ok, type: r.headers.get("content-type"), used: r.bodyUsed });
const copy = r.clone();
show("text", [await r.text(), await copy.text(), r.bodyUsed]);
show("read twice", await r.text().then(() => "read", (e) => e.name));
show("json", await new Response('{"a":1}').json());
show("bytes", [...new Uint8Array(await new Response(new Uint8Array([1, 2, 3])).arrayBuffer())]);
show("null body", [new Response(null).body, await new Response(null).text(), new Response().status]);
const j = Response.json({ ok: true }, { status: 202 });
show("Response.json", { status: j.status, type: j.headers.get("content-type"), body: await j.text() });
const red = Response.redirect("https://h/x", 301);
show("redirect", { status: red.status, location: red.headers.get("location") });
for (const status of [199, 600]) { try { new Response("", { status }); show("status", "accepted"); } catch (e) { show(`status ${status}`, e.name); } }
const params = new Response(new URLSearchParams({ a: "1" }));
show("form body", { type: params.headers.get("content-type"), text: await params.text() });
const req = new Request("https://h/p?q", { method: "post", body: "data", headers: { "x-b": "2" } });
show("request", { url: req.url, method: req.method, type: req.headers.get("content-type"), text: await req.text(), signal: req.signal instanceof AbortSignal, redirect: req.redirect });
try { new Request("https://h/", { method: "GET", body: "x" }); show("get body", "accepted"); } catch (e) { show("get body", e.name); }
show("request copy", new Request(new Request("https://h/a", { method: "PUT" })).method);
