// Buffer creation, encodings and comparison.
import { Buffer } from "node:buffer";
const show = (label, value) => console.log(label, JSON.stringify(value));
const b = Buffer.from("héllo ✓");
show("utf8", { length: b.length, byteLength: Buffer.byteLength("héllo ✓"), text: b.toString(), utf8: b.toString("utf8"), dash: b.toString("utf-8") });
show("hex", { to: b.toString("hex"), from: Buffer.from("68656c6c6f", "hex").toString() });
show("base64", { to: Buffer.from("any carnal pleas").toString("base64"), from: Buffer.from("aGVsbG8=", "base64").toString() });
show("from array", Buffer.from([104, 105, 256, -1]).toJSON());
show("alloc", { zero: [...Buffer.alloc(3)], fill: [...Buffer.alloc(3, 7)], unsafe: Buffer.allocUnsafe(4).length });
show("concat", { joined: Buffer.concat([Buffer.from("ab"), Buffer.from("cd")]).toString(), limited: Buffer.concat([Buffer.from("ab"), Buffer.from("cd")], 3).toString() });
show("equals", [Buffer.from("x").equals(Buffer.from("x")), Buffer.from("x").equals(Buffer.from("y"))]);
show("isBuffer", [Buffer.isBuffer(b), Buffer.isBuffer(new Uint8Array(1)), b instanceof Uint8Array]);
show("toJSON", JSON.stringify(Buffer.from("hi")));
