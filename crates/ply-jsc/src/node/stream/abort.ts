// stream.addAbortSignal: destroys the stream with an AbortError when the
// signal aborts.
import { errors, kinds } from "./state";
import type { Base } from "./state";

export function addAbortSignal<T extends Base>(signal: AbortSignal, stream: T): T {
  if (!signal || typeof signal.aborted !== "boolean") {
    throw Object.assign(new TypeError('The "signal" argument must be an instance of AbortSignal.'), { code: "ERR_INVALID_ARG_TYPE" });
  }
  const abort = () => stream.destroy(errors.aborted(signal));
  if (signal.aborted) abort();
  else {
    signal.addEventListener("abort", abort, { once: true });
    stream.once("close", () => signal.removeEventListener("abort", abort));
  }
  return stream;
}
kinds.addAbortSignal = addAbortSignal;
