// node:buffer (RFC 0015). The prelude defines the Buffer class and its
// encodings; loading this module installs the rest of Buffer's API.
import type * as nodeBuffer from "node:buffer";
import { decode, encode } from "ply:runtime";
import { bytes, fillBytes, statics } from "./buffer/bytes";
import { numbers } from "./buffer/numbers";

Object.assign(Buffer.prototype, numbers, bytes, { fill: fillBytes, toLocaleString: Buffer.prototype.toString });
Object.assign(Buffer, statics);

export { Buffer };
export const kMaxLength = Number.MAX_SAFE_INTEGER;
export const kStringMaxLength = 2 ** 29 - 24;
export const INSPECT_MAX_BYTES = 50;
export const constants: typeof nodeBuffer.constants = { MAX_LENGTH: kMaxLength, MAX_STRING_LENGTH: kStringMaxLength };

const missing = () => Object.assign(new TypeError('The "data" argument must be specified'), { code: "ERR_MISSING_ARGS" });
const invalid = (message: string) => new DOMException(message, "InvalidCharacterError");

/** Latin-1 text to base64, as browsers define it. */
export const btoa: typeof nodeBuffer.btoa = function (...args: unknown[]) {
  if (args.length === 0) throw missing();
  const text = String(args[0]);
  for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) > 255) throw invalid("Invalid character");
  return decode(encode(text, "latin1"), "base64");
};

/** Base64 to Latin-1 text, rejecting what browsers reject. */
export const atob: typeof nodeBuffer.atob = function (...args: unknown[]) {
  if (args.length === 0) throw missing();
  const text = String(args[0]).replace(/[\t\n\f\r ]/g, "");
  if (text.length % 4 === 1 || /[^A-Za-z0-9+/=]/.test(text) || /=[^=]/.test(text) || /===$/.test(text)) {
    throw invalid("The string to be decoded is not correctly encoded.");
  }
  return decode(encode(text, "base64"), "latin1");
};

const bytesOf = (input: NodeJS.TypedArray | ArrayBuffer) => (input instanceof ArrayBuffer ? new Uint8Array(input) : new Uint8Array(input.buffer, input.byteOffset, input.byteLength));

/** Well-formed UTF-8: no overlong forms, surrogates or values past U+10FFFF. */
export const isUtf8: typeof nodeBuffer.isUtf8 = (input) => {
  const b = bytesOf(input);
  for (let i = 0; i < b.length;) {
    const c = b[i];
    const [n, lo, hi] = c < 0x80 ? [0, 0, 0] : c >= 0xc2 && c <= 0xdf ? [1, 0x80, 0xbf] : c === 0xe0 ? [2, 0xa0, 0xbf] : c === 0xed ? [2, 0x80, 0x9f]
      : c >= 0xe1 && c <= 0xef ? [2, 0x80, 0xbf] : c === 0xf0 ? [3, 0x90, 0xbf] : c >= 0xf1 && c <= 0xf3 ? [3, 0x80, 0xbf] : c === 0xf4 ? [3, 0x80, 0x8f] : [-1, 0, 0];
    if (n < 0 || i + n >= b.length) return false;
    if (n && (b[i + 1] < lo || b[i + 1] > hi)) return false;
    for (let j = 2; j <= n; j++) if ((b[i + j] & 0xc0) !== 0x80) return false;
    i += n + 1;
  }
  return true;
};

export const isAscii: typeof nodeBuffer.isAscii = (input) => bytesOf(input).every((b) => b < 0x80);
