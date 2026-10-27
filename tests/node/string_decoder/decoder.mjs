// StringDecoder never splits a character across writes.
import { StringDecoder } from "node:string_decoder";
const show = (label, value) => console.log(label, JSON.stringify(value));
const pieces = (encoding, buffer, sizes) => {
  const d = new StringDecoder(encoding);
  const out = [];
  let at = 0;
  for (const size of sizes) { out.push(d.write(buffer.subarray(at, at + size))); at += size; }
  out.push(d.end(buffer.subarray(at)));
  return out;
};
const text = Buffer.from("aé€😀");
show("utf8 split", pieces("utf8", text, [2, 1, 2, 3]));
show("utf8 incomplete end", [new StringDecoder("utf8").end(Buffer.from([0xe2, 0x82])), new StringDecoder().write(Buffer.from([0xff, 0x41]))]);
show("utf16le", pieces("utf16le", Buffer.from("a😀b", "utf16le"), [1, 2, 3]));
show("base64", pieces("base64", Buffer.from("hello world"), [4, 4]));
show("hex and latin1", [new StringDecoder("hex").write(Buffer.from([1, 255])), new StringDecoder("latin1").write(Buffer.from([0xe9]))]);
show("encoding names", ["utf-8", "UCS2", "binary", "base64url"].map((e) => new StringDecoder(e).encoding));
try { new StringDecoder("nope"); } catch (e) { show("unknown", e.code); }
show("strings pass through", new StringDecoder().write("already text"));
