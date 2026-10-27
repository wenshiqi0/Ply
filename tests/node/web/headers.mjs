// Headers normalisation, combination and iteration order.
const show = (label, value) => console.log(label, JSON.stringify(value));
const h = new Headers({ "Content-Type": "text/plain", Accept: "a" });
h.append("accept", "b");
h.append("Set-Cookie", "x=1");
h.append("set-cookie", "y=2");
h.set("X-Custom", "  padded  ");
show("get", { accept: h.get("ACCEPT"), cookie: h.get("set-cookie"), custom: h.get("x-custom"), missing: h.get("nope") });
show("setCookie", h.getSetCookie());
show("entries", [...h]);
show("keys and values", [[...h.keys()], [...h.values()]]);
h.delete("accept");
show("has", [h.has("Accept"), h.has("content-type")]);
show("from pairs", [...new Headers([["a", "1"], ["A", "2"]])]);
show("from headers", [...new Headers(h)].length);
for (const bad of [["bad name", "v"], ["x", "a\nb"]]) { try { new Headers([bad]); show("bad", "accepted"); } catch (e) { show("bad", e.name); } }
const seen = [];
h.forEach((value, name) => seen.push(name));
show("forEach", seen);
