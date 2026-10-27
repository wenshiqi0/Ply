// Ordering of nextTick, promise jobs, queueMicrotask and timers.
const order = [];
setTimeout(() => order.push("timeout"), 0);
Promise.resolve().then(() => order.push("promise"));
queueMicrotask(() => order.push("microtask"));
process.nextTick(() => order.push("nextTick"));
process.nextTick((a, b) => order.push(`nextTick args ${a}${b}`), "x", "y");
order.push("sync");
setTimeout(() => console.log(JSON.stringify(order)), 5);
