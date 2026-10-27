// node:stream (RFC 0015). Like `require("stream")`, the module is the legacy
// Stream constructor carrying every class and helper.
import type * as nodeStream from "node:stream";
import { Stream } from "./stream/legacy";
import { Readable } from "./stream/readable";
import "./stream/readable-events";
import "./stream/readable-pipe";
import { from } from "./stream/readable-iter";
import { Writable } from "./stream/writable";
import "./stream/writable-end";
import { Duplex, PassThrough, Transform } from "./stream/duplex";
import { finished, finishedPromise } from "./stream/finished";
import { pipeline, pipelinePromise } from "./stream/pipeline";
import { addAbortSignal } from "./stream/abort";
import { getDefaultHighWaterMark, setDefaultHighWaterMark } from "./stream/state";

export const promises = { pipeline: pipelinePromise, finished: finishedPromise };
const isErrored = (stream: { _readableState?: { errored: unknown }; _writableState?: { errored: unknown } }) =>
  !!(stream._readableState?.errored || stream._writableState?.errored);
const isReadable = (stream: { readable?: boolean; destroyed?: boolean }) => stream.readable === true && !stream.destroyed;

Object.defineProperty(finished, Symbol.for("nodejs.util.promisify.custom"), { value: finishedPromise });
Object.defineProperty(pipeline, Symbol.for("nodejs.util.promisify.custom"), { value: pipelinePromise });
Object.assign(Stream, {
  Stream, Readable, Writable, Duplex, Transform, PassThrough, pipeline, finished, addAbortSignal, promises,
  getDefaultHighWaterMark, setDefaultHighWaterMark, isErrored, isReadable,
  _isUint8Array: (value: unknown) => value instanceof Uint8Array,
  _uint8ArrayToBuffer: (value: Uint8Array) => Buffer.from(value.buffer, value.byteOffset, value.byteLength),
});
void from;

export { Stream, Readable, Writable, Duplex, Transform, PassThrough, pipeline, finished, addAbortSignal, getDefaultHighWaterMark, setDefaultHighWaterMark, isErrored, isReadable };
export default Stream as unknown as typeof nodeStream;
