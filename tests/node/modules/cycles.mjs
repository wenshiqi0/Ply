// Imported bindings are live, including across an import cycle.
import { a, setA, seenFromA } from "./cycle/a.mjs";
import { readA, early } from "./cycle/b.mjs";
const before = a;
setA("a1");
console.log(JSON.stringify({ before, after: a, readA: readA(), seenFromA, early }));
