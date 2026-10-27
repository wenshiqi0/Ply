// Buffer's numeric reads and writes: fixed widths through DataView, and
// the 1-to-6-byte integers. Their signatures are @types/node's.

const outOfRange = (name: string, range: string, value: unknown) => Object.assign(
  new RangeError(`The value of "${name}" is out of range. It must be ${range}. Received ${String(value)}`), { code: "ERR_OUT_OF_RANGE" });

export function checkOffset(buf: Uint8Array, offset: unknown, size: number): number {
  if (offset === undefined) offset = 0;
  if (typeof offset !== "number") {
    throw Object.assign(new TypeError(`The "offset" argument must be of type number. Received type ${typeof offset}`), { code: "ERR_INVALID_ARG_TYPE" });
  }
  if (!Number.isInteger(offset)) throw outOfRange("offset", "an integer", offset);
  if (offset < 0 || offset + size > buf.length) {
    if (buf.length < size) throw Object.assign(new RangeError("Attempt to access memory outside buffer bounds"), { code: "ERR_BUFFER_OUT_OF_BOUNDS" });
    throw outOfRange("offset", `>= 0 and <= ${buf.length - size}`, offset);
  }
  return offset;
}

/** Buffer's own methods, checked against @types/node (Object's are not). */
export type BufferMethods = Partial<Omit<Buffer, "valueOf" | "toString" | "toLocaleString">>;

const view = (buf: Uint8Array) => new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
type Wide = "Uint16" | "Int16" | "Uint32" | "Int32" | "Float32" | "Float64";
type Big = "BigUint64" | "BigInt64";

function byte(signed: boolean) {
  return {
    read(this: Buffer, offset?: number): number {
      offset = checkOffset(this, offset, 1);
      return signed ? view(this).getInt8(offset) : this[offset];
    },
    write(this: Buffer, value: number, offset?: number): number {
      offset = checkOffset(this, offset, 1);
      const [min, max] = signed ? [-0x80, 0x7f] : [0, 0xff];
      if (value < min || value > max) throw outOfRange("value", `>= ${min} and <= ${max}`, value);
      if (signed) view(this).setInt8(offset, value); else this[offset] = value;
      return offset + 1;
    },
  };
}

function wide(kind: Wide, size: number, little: boolean) {
  const limits: Partial<Record<Wide, [number, number]>> = { Uint16: [0, 0xffff], Int16: [-0x8000, 0x7fff], Uint32: [0, 0xffffffff], Int32: [-0x80000000, 0x7fffffff] };
  return {
    read(this: Buffer, offset?: number): number { return view(this)[`get${kind}`](checkOffset(this, offset, size), little); },
    write(this: Buffer, value: number, offset?: number): number {
      offset = checkOffset(this, offset, size);
      const range = limits[kind];
      if (range && (value < range[0] || value > range[1])) throw outOfRange("value", `>= ${range[0]} and <= ${range[1]}`, value);
      view(this)[`set${kind}`](offset, +value, little);
      return offset + size;
    },
  };
}

function big(kind: Big, little: boolean) {
  const [min, max] = kind === "BigUint64" ? [0n, 2n ** 64n - 1n] : [-(2n ** 63n), 2n ** 63n - 1n];
  return {
    read(this: Buffer, offset?: number): bigint { return view(this)[`get${kind}`](checkOffset(this, offset, 8), little); },
    write(this: Buffer, value: bigint, offset?: number): number {
      offset = checkOffset(this, offset, 8);
      if (typeof value !== "bigint") throw Object.assign(new TypeError(`The "value" argument must be of type bigint. Received type ${typeof value}`), { code: "ERR_INVALID_ARG_TYPE" });
      if (value < min || value > max) throw outOfRange("value", `>= ${min}n and <= ${max}n`, `${value}n`);
      view(this)[`set${kind}`](offset, value, little);
      return offset + 8;
    },
  };
}

function byteLengthOf(n: number): number {
  if (!Number.isInteger(n) || n < 1 || n > 6) throw outOfRange("byteLength", ">= 1 and <= 6", n);
  return n;
}
function variable(little: boolean) {
  const at = (i: number, n: number) => (little ? i : n - 1 - i);
  function readUnsigned(this: Buffer, offset: number, length: number): number {
    const n = byteLengthOf(length);
    offset = checkOffset(this, offset, n);
    let value = 0;
    for (let i = n - 1; i >= 0; i--) value = value * 256 + this[offset + at(i, n)];
    return value;
  }
  function writeUnsigned(this: Buffer, value: number, offset: number, length: number): number {
    const n = byteLengthOf(length);
    offset = checkOffset(this, offset, n);
    const max = 2 ** (8 * n) - 1;
    if (value < 0 || value > max) throw outOfRange("value", `>= 0 and <= ${max}`, value);
    for (let i = 0; i < n; i++) { this[offset + at(i, n)] = value % 256; value = Math.floor(value / 256); }
    return offset + n;
  }
  return {
    readUnsigned,
    readSigned(this: Buffer, offset: number, length: number): number {
      const value = readUnsigned.call(this, offset, length);
      const limit = 2 ** (8 * length - 1);
      return value >= limit ? value - 2 * limit : value;
    },
    writeUnsigned,
    writeSigned(this: Buffer, value: number, offset: number, length: number): number {
      const limit = 2 ** (8 * byteLengthOf(length) - 1);
      if (value < -limit || value > limit - 1) throw outOfRange("value", `>= ${-limit} and <= ${limit - 1}`, value);
      return writeUnsigned.call(this, value < 0 ? value + 2 * limit : value, offset, length);
    },
  };
}

const [u8, i8] = [byte(false), byte(true)];
const [u16le, u16be, i16le, i16be] = [wide("Uint16", 2, true), wide("Uint16", 2, false), wide("Int16", 2, true), wide("Int16", 2, false)];
const [u32le, u32be, i32le, i32be] = [wide("Uint32", 4, true), wide("Uint32", 4, false), wide("Int32", 4, true), wide("Int32", 4, false)];
const [f32le, f32be, f64le, f64be] = [wide("Float32", 4, true), wide("Float32", 4, false), wide("Float64", 8, true), wide("Float64", 8, false)];
const [bu64le, bu64be, bi64le, bi64be] = [big("BigUint64", true), big("BigUint64", false), big("BigInt64", true), big("BigInt64", false)];
const [le, be] = [variable(true), variable(false)];

/** Installed on Buffer.prototype; Node spells unsigned names both UInt and Uint. */
export const numbers = {
  readUInt8: u8.read, readUint8: u8.read, writeUInt8: u8.write, writeUint8: u8.write, readInt8: i8.read, writeInt8: i8.write,
  readUInt16LE: u16le.read, readUint16LE: u16le.read, readUInt16BE: u16be.read, readUint16BE: u16be.read,
  writeUInt16LE: u16le.write, writeUint16LE: u16le.write, writeUInt16BE: u16be.write, writeUint16BE: u16be.write,
  readInt16LE: i16le.read, readInt16BE: i16be.read, writeInt16LE: i16le.write, writeInt16BE: i16be.write,
  readUInt32LE: u32le.read, readUint32LE: u32le.read, readUInt32BE: u32be.read, readUint32BE: u32be.read,
  writeUInt32LE: u32le.write, writeUint32LE: u32le.write, writeUInt32BE: u32be.write, writeUint32BE: u32be.write,
  readInt32LE: i32le.read, readInt32BE: i32be.read, writeInt32LE: i32le.write, writeInt32BE: i32be.write,
  readFloatLE: f32le.read, readFloatBE: f32be.read, writeFloatLE: f32le.write, writeFloatBE: f32be.write,
  readDoubleLE: f64le.read, readDoubleBE: f64be.read, writeDoubleLE: f64le.write, writeDoubleBE: f64be.write,
  readBigUInt64LE: bu64le.read, readBigUint64LE: bu64le.read, readBigUInt64BE: bu64be.read, readBigUint64BE: bu64be.read,
  writeBigUInt64LE: bu64le.write, writeBigUint64LE: bu64le.write, writeBigUInt64BE: bu64be.write, writeBigUint64BE: bu64be.write,
  readBigInt64LE: bi64le.read, readBigInt64BE: bi64be.read, writeBigInt64LE: bi64le.write, writeBigInt64BE: bi64be.write,
  readUIntLE: le.readUnsigned, readUintLE: le.readUnsigned, readUIntBE: be.readUnsigned, readUintBE: be.readUnsigned,
  readIntLE: le.readSigned, readIntBE: be.readSigned,
  writeUIntLE: le.writeUnsigned, writeUintLE: le.writeUnsigned, writeUIntBE: be.writeUnsigned, writeUintBE: be.writeUnsigned,
  writeIntLE: le.writeSigned, writeIntBE: be.writeSigned,
} satisfies BufferMethods;
