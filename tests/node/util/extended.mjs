// util's helpers beyond format and inspect.
import util from "node:util";
import types from "node:util/types";
const show = (label, value) => console.log(label, JSON.stringify(value));

function Base() {}
Base.prototype.hello = function () { return "hi"; };
function Child() {}
util.inherits(Child, Base);
show("inherits", [new Child().hello(), Child.super_ === Base, new Child() instanceof Base]);
const old = util.deprecate((x) => x * 2, "old() is deprecated", "DEP_PLY");
show("deprecate", [old(2), old(3)]);
const callbackified = util.callbackify(async (x) => { if (x < 0) throw new Error("neg"); return x + 1; });
show("callbackify", await Promise.all([1, -1].map((x) => new Promise((resolve) => callbackified(x, (err, value) => resolve([err?.message ?? null, value ?? null]))))));
show("debuglog", [util.debuglog("plycompat").enabled, typeof util.debuglog("plycompat")]);
show("styleText", [util.styleText("red", "plain"), util.styleText(["bold", "red"], "x", { validateStream: false }).split("\u001b").join("ESC")]);
try { util.styleText("nope", "x"); } catch (e) { show("styleText invalid", e.code); }
show("strip", util.stripVTControlCharacters("\u001b[31mred\u001b[39m \u001b]8;;link\u0007text"));
show("toUSVString", util.toUSVString("a\ud800b"));
show("isArray", [util.isArray([]), util.isArray({})]);
show("formatWithOptions", util.formatWithOptions({ colors: false }, "%s=%d", "n", 4));
show("custom symbols", [util.inspect.custom === Symbol.for("nodejs.util.inspect.custom"), util.promisify.custom === Symbol.for("nodejs.util.promisify.custom")]);
const custom = Object.assign(() => {}, { [util.promisify.custom]: () => Promise.resolve("custom") });
show("promisify custom", await util.promisify(custom)());
show("types", {
  map: types.isMap(new Map()), set: types.isSet(new Set()), weak: types.isWeakMap(new WeakMap()), typed: types.isTypedArray(new Uint8Array(1)),
  dataView: types.isDataView(new DataView(new ArrayBuffer(1))), view: types.isArrayBufferView(new DataView(new ArrayBuffer(1))), buffer: types.isArrayBuffer(new ArrayBuffer(1)),
  boxed: [types.isBoxedPrimitive(Object(1)), types.isNumberObject(1), types.isStringObject(new String("s"))], error: types.isNativeError(new TypeError()),
  generator: types.isGeneratorFunction(function* () {}), iterator: types.isMapIterator(new Map().keys()), args: (function () { return types.isArgumentsObject(arguments); })(),
  same: util.types === types,
});
