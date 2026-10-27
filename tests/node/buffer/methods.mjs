// Buffer's read/write family, search, copy, comparison and encodings.
import { Buffer, atob, btoa, constants, isAscii, isUtf8, kMaxLength } from "node:buffer";
const show = (label, value) => console.log(label, JSON.stringify(value, (k, v) => (typeof v === "bigint" ? `${v}n` : v)));
const b = Buffer.alloc(16);
const writes = [b.writeUInt8(255, 0), b.writeInt8(-2, 1), b.writeUInt16LE(0x1234, 2), b.writeInt16BE(-300, 4), b.writeUInt32LE(0xdeadbeef, 6), b.writeInt32BE(-5, 10), b.writeUIntBE(0x123456, 13, 3)];
show("writes", { offsets: writes, hex: b.toString("hex") });
show("reads", [b.readUInt8(0), b.readInt8(1), b.readUInt16LE(2), b.readUint16BE(2), b.readInt16BE(4), b.readUInt32LE(6), b.readInt32BE(10), b.readUIntBE(13, 3), b.readIntLE(13, 3)]);
const f = Buffer.alloc(16);
f.writeFloatLE(1.5, 0); f.writeDoubleBE(-0.25, 4); f.writeBigInt64LE(-(2n ** 40n), 8);
show("floats and bigints", [f.readFloatLE(0), f.readDoubleBE(4), f.readBigInt64LE(8), f.readBigUInt64BE(8)]);
for (const attempt of [() => b.readUInt32LE(14), () => b.writeUInt8(256, 0), () => b.readUInt8(1.5), () => b.writeIntLE(1, 0, 7)]) {
  try { attempt(); show("range", "ok"); } catch (e) { show("range", { name: e.name, code: e.code, message: e.message }); }
}
const w = Buffer.alloc(5);
show("write", [w.write("héllo"), w.toString(), Buffer.alloc(4).write("ab", 1, "latin1"), Buffer.alloc(2).write("€")]);
const source = Buffer.from("abcdef");
const target = Buffer.alloc(4, ".");
show("copy", [source.copy(target, 1, 2, 4), target.toString(), source.copy(Buffer.alloc(2))]);
show("compare", [Buffer.compare(Buffer.from("a"), Buffer.from("b")), Buffer.from("b").compare(Buffer.from("a")), [Buffer.from("c"), Buffer.from("a"), Buffer.from("b")].sort(Buffer.compare).map(String)]);
const hay = Buffer.from("hello world, hello");
show("search", [hay.indexOf("hello"), hay.indexOf("hello", 1), hay.lastIndexOf("hello"), hay.indexOf(Buffer.from("world")), hay.indexOf(111), hay.includes("xyz"), hay.indexOf("hello", -5), hay.indexOf("")]);
show("fill", [Buffer.alloc(5).fill("ab").toString(), Buffer.alloc(4).fill("6869", "hex").toString(), Buffer.alloc(3).fill(257).toJSON().data]);
show("swap", [Buffer.from([1, 2, 3, 4]).swap16().toJSON().data, Buffer.from([1, 2, 3, 4]).swap32().toJSON().data]);
const parent = Buffer.from("view");
const view = parent.slice(1, 3);
view[0] = 0x45;
show("slice is a view", [parent.toString(), view.toString(), parent.subarray(2) instanceof Buffer]);
show("encodings", [Buffer.from("hé", "latin1").toJSON().data, Buffer.from("hi", "utf16le").toJSON().data, Buffer.from([0xe9, 0x41]).toString("ascii"), Buffer.from("aGk", "base64url").toString(), Buffer.from("zz12", "hex").length]);
show("isEncoding", ["utf8", "UTF-8", "ucs2", "base64url", "nope", ""].map((e) => Buffer.isEncoding(e)));
show("atob and btoa", [btoa("Ply"), atob("UGx5"), atob(" UG x5 ")]);
for (const bad of [() => btoa("€"), () => atob("@@")]) { try { bad(); } catch (e) { show("invalid", { name: e.name, isDOMException: e instanceof DOMException }); } }
show("utf8 and ascii checks", [isUtf8(Buffer.from("é")), isUtf8(Buffer.from([0xc3])), isUtf8(Buffer.from([0xed, 0xa0, 0x80])), isAscii(Buffer.from("abc")), isAscii(Buffer.from("é"))]);
show("limits", { kMaxLength, max: constants.MAX_LENGTH, string: constants.MAX_STRING_LENGTH });
