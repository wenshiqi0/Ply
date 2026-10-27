// Timer ordering by delay, arguments, clearing and timers/promises.
import { setTimeout as sleep } from "node:timers/promises";
import timers from "node:timers";
const show = (label, value) => console.log(label, JSON.stringify(value));
const order = [];
setTimeout(() => order.push("t20"), 20);
setTimeout(() => order.push("t5"), 5);
setTimeout((a, b) => order.push(`args ${a} ${b}`), 10, "x", "y");
const cancelled = setTimeout(() => order.push("cancelled"), 1);
clearTimeout(cancelled);
let ticks = 0;
const interval = setInterval(() => { if (++ticks === 3) clearInterval(interval); }, 2);
setImmediate(() => order.push("immediate"));
show("module", timers.setTimeout === setTimeout);
show("sleep value", await sleep(30, "slept"));
show("order", order.filter((x) => x !== "immediate"));
show("immediate ran", order.includes("immediate"));
show("interval ticks", ticks);
