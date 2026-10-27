// Buffer's byte operations and static helpers beyond the prelude's class.
import { encode, toBuffer } from "ply:runtime";
import type { BufferMethods } from "./numbers";

const ENCODINGS = ["utf8", "utf-8", "hex", "base64", "base64url", "ascii", "latin1", "binary", "ucs2", "ucs-2", "utf16le", "utf-16le"];
const isEncoding = (encoding: string): encoding is BufferEncoding => typeof encoding === "string" && ENCODINGS.includes(encoding.toLowerCase());
const asBytes = (value: string | Uint8Array, encoding?: BufferEncoding) => (typeof value === "string" ? encode(value, encoding ?? "utf8") : value);

function compare(a: Uint8Array, b: Uint8Array): -1 | 0 | 1 {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  return a.length === b.length ? 0 : a.length < b.length ? -1 : 1;
}

const uint8 = Uint8Array.prototype;
function search(buf: Uint8Array, value: string | number | Uint8Array, from: number | BufferEncoding | undefined, encoding: BufferEncoding | undefined, last: boolean): number {
  if (typeof from === "string") [encoding, from] = [from, undefined];
  if (typeof value === "number") {
    const byte = value & 255;
    return from === undefined ? (last ? uint8.lastIndexOf.call(buf, byte) : uint8.indexOf.call(buf, byte)) : last ? uint8.lastIndexOf.call(buf, byte, from) : uint8.indexOf.call(buf, byte, from);
  }
  const needle = asBytes(value, encoding);
  const start = from === undefined ? (last ? buf.length - needle.length : 0) : from < 0 ? buf.length + from : from;
  if (needle.length === 0) return Math.min(Math.max(start, 0), buf.length);
  const step = last ? -1 : 1;
  for (let i = last ? Math.min(start, buf.length - needle.length) : Math.max(start, 0); last ? i >= 0 : i <= buf.length - needle.length; i += step) {
    let j = 0;
    while (j < needle.length && buf[i + j] === needle[j]) j++;
    if (j === needle.length) return i;
  }
  return -1;
}

function swap(size: number) {
  return function (this: Buffer): Buffer {
    if (this.length % size) throw Object.assign(new RangeError(`Buffer size must be a multiple of ${size * 8}-bits`), { code: "ERR_INVALID_BUFFER_SIZE" });
    for (let i = 0; i < this.length; i += size) this.subarray(i, i + size).reverse();
    return this;
  };
}

/** Installed on Buffer.prototype. */
export const bytes = {
  write(this: Buffer, string: string, offset?: number | BufferEncoding, length?: number | BufferEncoding, encoding?: BufferEncoding): number {
    if (typeof offset === "string") [encoding, offset, length] = [offset, 0, undefined];
    else if (typeof length === "string") [encoding, length] = [length, undefined];
    const at = offset ?? 0;
    const max = Math.min(this.length - at, length ?? this.length - at);
    let data = encode(String(string), encoding ?? "utf8");
    // Never write part of a UTF-8 character.
    if (data.length > max) {
      let end = max;
      if (!encoding || /^utf-?8$/i.test(encoding)) while (end > 0 && (data[end] & 0xc0) === 0x80) end--;
      data = data.subarray(0, end);
    }
    this.set(data, at);
    return data.length;
  },
  copy(this: Buffer, target: Uint8Array, targetStart = 0, sourceStart = 0, sourceEnd = this.length): number {
    const count = Math.max(0, Math.min(sourceEnd - sourceStart, target.length - targetStart, this.length - sourceStart));
    target.set(this.subarray(sourceStart, sourceStart + count), targetStart);
    return count;
  },
  compare(this: Buffer, target: Uint8Array, targetStart = 0, targetEnd = target.length, sourceStart = 0, sourceEnd = this.length): -1 | 0 | 1 {
    return compare(this.subarray(sourceStart, sourceEnd), target.subarray(targetStart, targetEnd));
  },
  equals(this: Buffer, other: Uint8Array): boolean {
    if (!(other instanceof Uint8Array)) throw Object.assign(new TypeError('The "otherBuffer" argument must be an instance of Buffer or Uint8Array.'), { code: "ERR_INVALID_ARG_TYPE" });
    return compare(this, other) === 0;
  },
  // Node's Buffer#slice is a view, like subarray (Uint8Array#slice copies).
  slice(this: Buffer<ArrayBuffer>, start?: number, end?: number): Buffer<ArrayBuffer> { return this.subarray(start, end); },
  indexOf(this: Buffer, value: string | number | Uint8Array, from?: number | BufferEncoding, encoding?: BufferEncoding): number { return search(this, value, from, encoding, false); },
  lastIndexOf(this: Buffer, value: string | number | Uint8Array, from?: number | BufferEncoding, encoding?: BufferEncoding): number { return search(this, value, from, encoding, true); },
  includes(this: Buffer, value: string | number | Buffer, from?: number | BufferEncoding, encoding?: BufferEncoding): boolean { return search(this, value, from, encoding, false) !== -1; },
  swap16: swap(2),
  swap32: swap(4),
  swap64: swap(8),
} satisfies BufferMethods;

const fill = uint8.fill;
/** Buffer#fill with strings, buffers and encodings, kept apart for its `this` return type. */
export function fillBytes(this: Buffer, value: string | Uint8Array | number | boolean, offset: number | BufferEncoding = 0, end: number | BufferEncoding = this.length, encoding?: BufferEncoding): Buffer {
  if (typeof offset === "string") [encoding, offset, end] = [offset, 0, this.length];
  if (typeof end === "string") [encoding, end] = [end, this.length];
  if (typeof value === "number" || typeof value === "boolean") return fill.call(this, Number(value) & 255, offset, end) as Buffer;
  const data = asBytes(value, encoding);
  if (data.length === 0) return fill.call(this, 0, offset, end) as Buffer;
  for (let i = offset; i < end; i++) this[i] = data[(i - offset) % data.length];
  return this;
}

/** Static members of Buffer the prelude's class lacks. */
export const statics = {
  alloc(size: number, value?: string | Uint8Array | number, encoding?: BufferEncoding): Buffer<ArrayBuffer> {
    const buf = toBuffer(new Uint8Array(size)) as Buffer<ArrayBuffer>;
    if (value !== undefined && value !== 0) fillBytes.call(buf, value, 0, size, encoding);
    return buf;
  },
  allocUnsafeSlow(size: number): Buffer<ArrayBuffer> { return statics.alloc(size); },
  compare,
  isEncoding,
  copyBytesFrom(view: NodeJS.TypedArray, offset = 0, length?: number): Buffer<ArrayBuffer> {
    const size = view.BYTES_PER_ELEMENT;
    const all = new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
    return toBuffer(all.slice(offset * size, length === undefined ? undefined : (offset + length) * size)) as Buffer<ArrayBuffer>;
  },
  poolSize: 8192,
} satisfies Partial<BufferConstructor>;
