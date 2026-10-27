// WHATWG URL parsing, setters and URLSearchParams.
const show = (label, value) => console.log(label, JSON.stringify(value));
const parts = (u) => ({ href: u.href, origin: u.origin, protocol: u.protocol, username: u.username, password: u.password, host: u.host, hostname: u.hostname, port: u.port, pathname: u.pathname, search: u.search, hash: u.hash });
for (const [input, base] of [["https://user:pw@EXAMPLE.com:443/a/./b/../c?x=1#f"], ["../d?q", "http://h/a/b/c"], ["//other/x", "https://h/"], ["file:///tmp/a b"], ["http://[::1]:8080/"], ["https://xn--fsq.com/"], ["https://例え.jp/パス"], ["mailto:someone@example.com"], ["blob:https://a/b"], ["http://h:65536"], ["not a url"], ["http://a b"]]) {
  try { show(`parse ${input}`, parts(base === undefined ? new URL(input) : new URL(input, base))); } catch (e) { show(`parse ${input}`, `${e.name} ${e.code}`); }
}
show("canParse", [URL.canParse("https://x"), URL.canParse("x"), URL.canParse("x", "https://h/")]);
const u = new URL("https://h/p?a=1&b=2#frag");
u.protocol = "http"; u.hostname = "example.org"; u.port = "8080"; u.pathname = "/q r"; u.hash = "";
u.searchParams.append("c", "3 4"); u.searchParams.delete("a");
show("setters", u.href);
u.search = "?z=9";
show("search updates params", [...u.searchParams]);
u.port = "http-default"; u.protocol = "nope:";
show("ignored setters", u.href);
const p = new URLSearchParams("a=1&b=%20x+y&a=2&empty=&flag");
show("params", { get: p.get("a"), all: p.getAll("a"), b: p.get("b"), empty: p.get("empty"), flag: p.get("flag"), missing: p.get("zz"), has: p.has("a", "2"), size: p.size });
p.set("a", "3"); p.sort();
show("params mutated", p.toString());
show("params init", [new URLSearchParams({ x: "1", y: "ä &=" }).toString(), new URLSearchParams([["k", "v"], ["k", "w"]]).toString(), new URLSearchParams("?lead=1").toString()]);
show("toJSON", JSON.stringify({ u: new URL("https://h/x") }));
