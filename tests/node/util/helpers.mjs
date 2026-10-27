// util.promisify, util.types and util.isDeepStrictEqual.
import util from "node:util";
const show = (label, value) => console.log(label, JSON.stringify(value));
const callback = (x, done) => setTimeout(() => (x < 0 ? done(new Error("negative")) : done(null, x * 2)), 1);
const promised = util.promisify(callback);
show("promisify", await promised(21));
show("promisify error", await promised(-1).catch((e) => e.message));
show("types", [util.types.isPromise(Promise.resolve()), util.types.isPromise({ then() {} }), util.types.isDate(new Date()), util.types.isRegExp(/x/), util.types.isDate("2020")]);
const eq = util.isDeepStrictEqual;
show("deep equal", [eq({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] }), eq([1, 2], [2, 1]), eq({ a: 1 }, { a: "1" }), eq(NaN, NaN), eq(new Date(0), new Date(0)),
  eq(new Map([[1, 2]]), new Map([[1, 2]])), eq(new Set([1]), new Set([2])), eq({ a: undefined }, {}), eq([1], { 0: 1 }), eq(0, -0)]);
show("TextEncoder", [util.TextEncoder === TextEncoder, util.TextDecoder === TextDecoder]);
