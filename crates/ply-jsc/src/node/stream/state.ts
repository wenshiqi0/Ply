// The state Node keeps for each side of a stream, and the errors streams
// report, with Node's codes and messages.
import type { StringDecoder } from "../string_decoder";

export type Chunk = unknown;
export type Callback = (error?: Error | null) => void;
export const nop = () => {};

export interface StreamOptions {
  highWaterMark?: number;
  objectMode?: boolean;
  readableObjectMode?: boolean;
  writableObjectMode?: boolean;
  readableHighWaterMark?: number;
  writableHighWaterMark?: number;
  encoding?: BufferEncoding | null;
  defaultEncoding?: BufferEncoding;
  decodeStrings?: boolean;
  emitClose?: boolean;
  autoDestroy?: boolean;
  allowHalfOpen?: boolean;
  signal?: AbortSignal;
}

let defaultHighWaterMark = 64 * 1024;
let defaultObjectHighWaterMark = 16;
export const getDefaultHighWaterMark = (objectMode: boolean) => (objectMode ? defaultObjectHighWaterMark : defaultHighWaterMark);
export function setDefaultHighWaterMark(objectMode: boolean, value: number): void {
  if (objectMode) defaultObjectHighWaterMark = value; else defaultHighWaterMark = value;
}

function highWaterMarkOf(options: StreamOptions | undefined, side: "readableHighWaterMark" | "writableHighWaterMark", duplex: boolean, objectMode: boolean): number {
  const value = options?.highWaterMark ?? (duplex ? options?.[side] : undefined);
  if (value === undefined || value === null) return getDefaultHighWaterMark(objectMode);
  if (!Number.isInteger(value) || value < 0) {
    throw Object.assign(new TypeError(`The property 'options.${duplex && options?.highWaterMark === undefined ? side : "highWaterMark"}' is invalid. Received ${value}`), { code: "ERR_INVALID_ARG_VALUE" });
  }
  return Math.floor(value);
}

export class ReadableState {
  objectMode: boolean;
  highWaterMark: number;
  buffer: Chunk[] = [];
  length = 0;
  pipes: Writable[] = [];
  flowing: boolean | null = null;
  ended = false;
  endEmitted = false;
  reading = false;
  constructed = true;
  sync = true;
  needReadable = false;
  emittedReadable = false;
  readableListening = false;
  resumeScheduled = false;
  paused: boolean | null = null;
  errorEmitted = false;
  emitClose: boolean;
  autoDestroy: boolean;
  destroyed = false;
  errored: Error | null = null;
  closed = false;
  closeEmitted = false;
  defaultEncoding: BufferEncoding;
  awaitDrainWriters: Writable | Set<Writable> | null = null;
  multiAwaitDrain = false;
  readingMore = false;
  dataEmitted = false;
  decoder: StringDecoder | null = null;
  encoding: BufferEncoding | null = null;
  readable?: boolean;

  constructor(options: StreamOptions | undefined, duplex: boolean) {
    this.objectMode = !!(options?.objectMode || (duplex && options?.readableObjectMode));
    this.highWaterMark = highWaterMarkOf(options, "readableHighWaterMark", duplex, this.objectMode);
    this.emitClose = options?.emitClose !== false;
    this.autoDestroy = options?.autoDestroy !== false;
    this.defaultEncoding = options?.defaultEncoding ?? "utf8";
  }
}

export type Buffered = { chunk: Chunk; encoding: BufferEncoding | "buffer" | ""; callback: Callback };
export class WritableState {
  objectMode: boolean;
  highWaterMark: number;
  finalCalled = false;
  needDrain = false;
  ending = false;
  ended = false;
  finished = false;
  destroyed = false;
  decodeStrings: boolean;
  defaultEncoding: BufferEncoding;
  length = 0;
  writing = false;
  corked = 0;
  sync = true;
  bufferProcessing = false;
  onwrite: Callback = nop;
  writecb: Callback | null = null;
  writelen = 0;
  afterWriteTickInfo: { count: number; cb: Callback } | null = null;
  buffered: (Buffered | null)[] = [];
  bufferedIndex = 0;
  allBuffers = true;
  allNoop = true;
  pendingcb = 0;
  constructed = true;
  prefinished = false;
  errorEmitted = false;
  emitClose: boolean;
  autoDestroy: boolean;
  errored: Error | null = null;
  closed = false;
  closeEmitted = false;
  onFinished: Callback[] = [];
  writable?: boolean;

  constructor(options: StreamOptions | undefined, duplex: boolean) {
    this.objectMode = !!(options?.objectMode || (duplex && options?.writableObjectMode));
    this.highWaterMark = highWaterMarkOf(options, "writableHighWaterMark", duplex, this.objectMode);
    this.decodeStrings = options?.decodeStrings !== false;
    this.defaultEncoding = options?.defaultEncoding ?? "utf8";
    this.emitClose = options?.emitClose !== false;
    this.autoDestroy = options?.autoDestroy !== false;
  }
}

/** What the stream modules need of each other's instances. */
export interface Base {
  _readableState?: ReadableState;
  _writableState?: WritableState;
  destroyed: boolean;
  emit(type: string | symbol, ...args: unknown[]): boolean;
  on(type: string | symbol, listener: (...args: never[]) => unknown): unknown;
  once(type: string | symbol, listener: (...args: never[]) => unknown): unknown;
  removeListener(type: string | symbol, listener: (...args: never[]) => unknown): unknown;
  listenerCount(type: string | symbol): number;
  destroy(error?: Error | null, callback?: Callback): unknown;
  _destroy(error: Error | null, callback: Callback): void;
  _construct?(callback: Callback): void;
}
export interface Writable extends Base {
  write(chunk: Chunk, callback?: Callback): boolean;
  end(callback?: () => void): unknown;
  writableNeedDrain?: boolean;
}

/** Stream modules recognise each other through these, filled in as they load. */
export const kinds: {
  Duplex?: new (...args: never[]) => object;
  addAbortSignal?: (signal: AbortSignal, stream: Base) => void;
} = {};

const coded = <E extends Error>(error: E, code: string) => Object.assign(error, { code });
export const errors = {
  pushAfterEof: () => coded(new Error("stream.push() after EOF"), "ERR_STREAM_PUSH_AFTER_EOF"),
  unshiftAfterEnd: () => coded(new Error("stream.unshift() after end event"), "ERR_STREAM_UNSHIFT_AFTER_END_EVENT"),
  nullValues: () => coded(new TypeError("May not write null values to stream"), "ERR_STREAM_NULL_VALUES"),
  writeAfterEnd: () => coded(new Error("write after end"), "ERR_STREAM_WRITE_AFTER_END"),
  destroyed: (method: string) => coded(new Error(`Cannot call ${method} after a stream was destroyed`), "ERR_STREAM_DESTROYED"),
  multipleCallback: () => coded(new Error("Callback called multiple times"), "ERR_MULTIPLE_CALLBACK"),
  alreadyFinished: (method: string) => coded(new Error(`Cannot call ${method} after a stream was finished`), "ERR_STREAM_ALREADY_FINISHED"),
  prematureClose: () => coded(new Error("Premature close"), "ERR_STREAM_PREMATURE_CLOSE"),
  notImplemented: (method: string) => coded(new Error(`The ${method} method is not implemented`), "ERR_METHOD_NOT_IMPLEMENTED"),
  unknownEncoding: (encoding: string) => coded(new TypeError(`Unknown encoding: ${encoding}`), "ERR_UNKNOWN_ENCODING"),
  chunkType: (chunk: unknown) => coded(new TypeError(`The "chunk" argument must be of type string or an instance of Buffer, TypedArray, or DataView.${describe(chunk)}`), "ERR_INVALID_ARG_TYPE"),
  aborted: (signal?: AbortSignal) => Object.assign(new Error("The operation was aborted", { cause: signal?.reason }), { name: "AbortError", code: "ABORT_ERR" }),
};
function describe(value: unknown): string {
  if (value === null || value === undefined) return ` Received ${value}`;
  if (typeof value === "function") return ` Received function ${value.name || "anonymous"}`;
  if (typeof value === "object") return ` Received an instance of ${value.constructor?.name ?? "Object"}`;
  return ` Received type ${typeof value} (${typeof value === "string" ? `'${value}'` : String(value)})`;
}
