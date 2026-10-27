// util.format placeholders and util.inspect of plain values.
import util from "node:util";
const show = (label, value) => console.log(label, JSON.stringify(value));
show("format", [
  util.format("%s and %s", "a", "b"), util.format("%d|%i|%f", "42.5", 42.9, "1.5"), util.format("%j", { a: [1] }),
  util.format("%%s %s", "x"), util.format("extra", 1, "two", null), util.format("%s", undefined), util.format(1, 2), util.format("%s"),
]);
show("inspect primitives", [util.inspect("s"), util.inspect(1), util.inspect(-0), util.inspect(null), util.inspect(undefined), util.inspect(true), util.inspect(10n), util.inspect(Symbol("x"))]);
show("inspect objects", [util.inspect({ a: 1, b: "two", c: [1, 2] }), util.inspect([]), util.inspect({}), util.inspect([1, [2, [3, [4, [5]]]]]), util.inspect({ a: { b: { c: { d: {} } } } })]);
show("inspect functions", [util.inspect(function named() {}), util.inspect(() => {}), util.inspect(class K {})]);
show("console.log formatting", [util.format({ a: 1 }), util.format("x", { y: [1] })]);
