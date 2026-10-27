// Node's own globals.
const show = (label, value) => console.log(label, JSON.stringify(value));
show("global", [global === globalThis, typeof global.setTimeout]);
show("base64 globals", [btoa("hi"), atob("aGk="), typeof atob]);
show("buffer and process", [typeof Buffer.from, typeof process.nextTick, typeof queueMicrotask, typeof structuredClone]);
