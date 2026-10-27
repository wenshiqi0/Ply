// node:string_decoder (RFC 0015): decodes a byte stream into strings without
// splitting characters across writes. UTF-8 and UTF-16 hold an incomplete
// character, base64 holds an incomplete group of three bytes.
import type * as nodeStringDecoder from "node:string_decoder";
import { decode } from "ply:runtime";

const NAMES: Record<string, string> = {
  utf8: "utf8", "utf-8": "utf8", ucs2: "utf16le", "ucs-2": "utf16le", utf16le: "utf16le", "utf-16le": "utf16le",
  base64: "base64", base64url: "base64url", hex: "hex", latin1: "latin1", binary: "latin1", ascii: "ascii",
};

/** Bytes at the end of `b` that start a UTF-8 character not yet complete. */
function utf8Tail(b: Uint8Array): number {
  for (let j = 1; j <= Math.min(3, b.length); j++) {
    const c = b[b.length - j];
    if ((c & 0xc0) === 0x80) continue;
    const need = c >= 0xf0 ? 4 : c >= 0xe0 ? 3 : c >= 0xc0 ? 2 : 1;
    return need > j ? j : 0;
  }
  return 0;
}

function join(a: Uint8Array, b: Uint8Array): Uint8Array {
  if (!a.length) return b;
  const out = new Uint8Array(a.length + b.length);
  out.set(a);
  out.set(b, a.length);
  return out;
}

export class StringDecoder implements nodeStringDecoder.StringDecoder {
  readonly encoding: BufferEncoding;
  #pending = new Uint8Array(0);

  constructor(encoding: string = "utf8") {
    const name = NAMES[String(encoding).toLowerCase()];
    if (!name) throw Object.assign(new TypeError(`Unknown encoding: ${encoding}`), { code: "ERR_UNKNOWN_ENCODING" });
    this.encoding = name as BufferEncoding;
  }

  write(buf: string | NodeJS.ArrayBufferView): string {
    if (typeof buf === "string") return buf;
    if (!ArrayBuffer.isView(buf)) {
      throw Object.assign(new TypeError('The "buf" argument must be an instance of Buffer, TypedArray, or DataView.'), { code: "ERR_INVALID_ARG_TYPE" });
    }
    const bytes = join(this.#pending, new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength));
    let keep = 0;
    if (this.encoding === "utf8") keep = utf8Tail(bytes);
    else if (this.encoding === "utf16le") {
      keep = bytes.length % 2;
      // A lead surrogate waits for its pair.
      const at = bytes.length - keep - 2;
      if (at >= 0 && (bytes[at + 1] & 0xfc) === 0xd8) keep += 2;
    } else if (this.encoding === "base64" || this.encoding === "base64url") keep = bytes.length % 3;
    this.#pending = bytes.slice(bytes.length - keep);
    return decode(bytes.subarray(0, bytes.length - keep), this.encoding);
  }

  end(buf?: string | NodeJS.ArrayBufferView): string {
    const out = buf === undefined ? "" : this.write(buf);
    const pending = this.#pending;
    this.#pending = new Uint8Array(0);
    if (!pending.length) return out;
    if (this.encoding === "utf8") return out + "�";
    if (this.encoding === "utf16le") return out + decode(pending.subarray(0, pending.length - (pending.length % 2)), "utf16le");
    return out + decode(pending, this.encoding);
  }
}
