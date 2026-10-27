// TextEncoder, TextDecoder and structuredClone.
const show = (label, value) => console.log(label, JSON.stringify(value));
const bytes = new TextEncoder().encode("é✓😀");
show("encode", [...bytes]);
show("decode", [new TextDecoder().decode(bytes), new TextDecoder().decode(new Uint8Array([0xff, 0x41])), new TextDecoder("utf-8").encoding]);
const original = { a: [1, { b: "c" }], d: null, n: 1.5 };
const cloned = structuredClone(original);
show("structuredClone", { equal: JSON.stringify(cloned) === JSON.stringify(original), distinct: cloned !== original && cloned.a !== original.a });
const rich = structuredClone({ date: new Date(0), map: new Map([[1, "one"]]), set: new Set([2]), re: /x/gi, big: 10n, undef: undefined });
show("structuredClone types", { date: rich.date instanceof Date && rich.date.getTime(), map: rich.map instanceof Map && rich.map.get(1), set: rich.set instanceof Set && rich.set.has(2), re: String(rich.re), big: typeof rich.big, undef: "undef" in rich });
const cyclic = { name: "loop" };
cyclic.self = cyclic;
const copy = structuredClone(cyclic);
show("structuredClone cycle", copy.self === copy);
try { structuredClone({ f() {} }); show("function", "cloned"); } catch (e) { show("function", e.name); }
