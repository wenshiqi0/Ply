// The legacy Stream base: an EventEmitter with the old pipe(), which every
// stream class inherits from. Like Node, it is a plain constructor.
import EventEmitter from "../events";
import type { Emitter } from "../events";

type Pipeable = Emitter & {
  write?(chunk: unknown): boolean;
  end?(): void;
  pause?(): void;
  resume?(): void;
  readable?: boolean;
  writable?: boolean;
  destroy?(): void;
  _isStdio?: boolean;
  on(type: string, listener: (...args: never[]) => unknown): unknown;
  removeListener(type: string, listener: (...args: never[]) => unknown): unknown;
};

export function Stream(this: object, options?: object): void {
  (EventEmitter as unknown as (this: Emitter, options?: object) => void).call(this as Emitter, options);
}
Object.setPrototypeOf(Stream.prototype, EventEmitter.prototype);
Object.setPrototypeOf(Stream, EventEmitter);

/** The pipe of streams that are not Readable: data events with drain backpressure. */
Stream.prototype.pipe = function (this: Pipeable, dest: Pipeable, options?: { end?: boolean }): Pipeable {
  return legacyPipe(this, dest, options);
};

function legacyPipe(source: Pipeable, dest: Pipeable, options?: { end?: boolean }): Pipeable {
  const ondata = (chunk: unknown) => { if (dest.writable && dest.write?.(chunk) === false) source.pause?.(); };
  const ondrain = () => { if (source.readable) source.resume?.(); };
  source.on("data", ondata);
  dest.on("drain", ondrain);
  let didOnEnd = false;
  const onend = () => { if (didOnEnd) return; didOnEnd = true; dest.end?.(); };
  const onclose = () => { if (didOnEnd) return; didOnEnd = true; dest.destroy?.(); };
  if (!dest._isStdio && options?.end !== false) {
    source.on("end", onend);
    source.on("close", onclose);
  }
  const onerror = (error: unknown) => {
    cleanup();
    if (EventEmitter.listenerCount(source as never, "error") === 0) source.emit("error", error);
  };
  source.on("error", onerror);
  dest.on("error", onerror);
  const cleanup = () => {
    source.removeListener("data", ondata);
    dest.removeListener("drain", ondrain);
    source.removeListener("end", onend);
    source.removeListener("close", onclose);
    source.removeListener("error", onerror);
    dest.removeListener("error", onerror);
    source.removeListener("end", cleanup);
    source.removeListener("close", cleanup);
    dest.removeListener("close", cleanup);
  };
  source.on("end", cleanup);
  source.on("close", cleanup);
  dest.on("close", cleanup);
  dest.emit("pipe", source);
  return dest;
}

export default Stream;
