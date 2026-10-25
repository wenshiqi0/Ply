// Private Web surface used by pi-ai's browser bundle. Socket/TLS work and
// response ownership live in Rust; a stream pull requests one native chunk.
import { Headers } from "headers-polyfill";
import { URL, URLSearchParams } from "whatwg-url";
// Its browser entry merely re-exports native globals, which bare JSC lacks.
import { AbortController, AbortSignal } from "abort-controller/dist/abort-controller.mjs";
import { ReadableStream } from "web-streams-polyfill";

const abortError = () => Object.assign(new Error("The operation was aborted"), { name: "AbortError" });
const bytes = value => {
  if (value === undefined || value === null) return undefined;
  if (typeof value === "string") return new TextEncoder().encode(value);
  if (value instanceof URLSearchParams) return new TextEncoder().encode(String(value));
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  throw new TypeError("embedded fetch supports text and byte request bodies");
};

// SDKs check instanceof FormData even for JSON requests. Keep its string
// collection semantics; multipart/file uploads are outside these providers.
class FormData {
  #entries = [];
  append(name, value) { this.#entries.push([String(name), String(value)]); }
  delete(name) { this.#entries = this.#entries.filter(([key]) => key !== String(name)); }
  get(name) { return this.#entries.find(([key]) => key === String(name))?.[1] ?? null; }
  getAll(name) { return this.#entries.filter(([key]) => key === String(name)).map(([, value]) => value); }
  has(name) { return this.#entries.some(([key]) => key === String(name)); }
  set(name, value) { this.delete(name); this.append(name, value); }
  entries() { return this.#entries[Symbol.iterator](); }
  *keys() { for (const [key] of this.#entries) yield key; }
  *values() { for (const [, value] of this.#entries) yield value; }
  forEach(fn, thisArg) { for (const [key, value] of this.#entries) fn.call(thisArg, value, key, this); }
  [Symbol.iterator]() { return this.entries(); }
}

class Request {
  constructor(input, init = {}) {
    const previous = input instanceof Request ? input : {};
    this.url = String(previous.url ?? input);
    this.method = String(init.method ?? previous.method ?? "GET").toUpperCase();
    this.headers = new Headers(init.headers ?? previous.headers);
    this.body = init.body === undefined ? previous.body : init.body;
    this.signal = init.signal ?? previous.signal;
    this.redirect = init.redirect ?? previous.redirect ?? "follow";
    if (!["follow", "manual", "error"].includes(this.redirect)) throw new TypeError("invalid redirect mode");
    if ((this.method === "GET" || this.method === "HEAD") && this.body != null) throw new TypeError("GET/HEAD cannot have a body");
  }
}

class Response {
  #used = false;
  constructor(body = null, init = {}) {
    this.status = init.status ?? 200;
    this.statusText = init.statusText ?? "";
    this.headers = new Headers(init.headers);
    this.url = init.url ?? "";
    this.redirected = init.redirected ?? false;
    this.type = "basic";
    this.body = body instanceof ReadableStream ? body : body == null ? null : new ReadableStream({
      start(controller) { controller.enqueue(bytes(body)); controller.close(); },
    });
  }
  get ok() { return this.status >= 200 && this.status < 300; }
  get bodyUsed() { return this.#used; }
  async arrayBuffer() {
    if (this.#used || this.body?.locked) throw new TypeError("response body is already in use");
    this.#used = true;
    if (!this.body) return new ArrayBuffer(0);
    const reader = this.body.getReader();
    const chunks = [];
    let length = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        length += value.byteLength;
      }
    } finally { reader.releaseLock(); }
    const output = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) { output.set(chunk, offset); offset += chunk.byteLength; }
    return output.buffer;
  }
  async text() { return new TextDecoder().decode(await this.arrayBuffer()); }
  async json() { return JSON.parse(await this.text()); }
  clone() {
    if (this.#used || this.body?.locked) throw new TypeError("response body is already in use");
    const [left, right] = this.body ? this.body.tee() : [null, null];
    this.body = left;
    return new Response(right, this);
  }
}

export function createWeb(host) {
  async function send(request, url) {
    if (request.signal?.aborted) throw abortError();
    const body = bytes(request.body);
    if (request.body instanceof URLSearchParams && !request.headers.has("content-type")) {
      request.headers.set("content-type", "application/x-www-form-urlencoded;charset=UTF-8");
    }
    const id = host.open({ url: String(url), method: request.method, headers: [...request.headers] }, body);
    let closed = false;
    let streamController;
    const close = () => {
      if (closed) return;
      closed = true;
      request.signal?.removeEventListener("abort", abort);
      host.abort(id);
    };
    const abort = () => {
      close();
      streamController?.error(abortError());
    };
    request.signal?.addEventListener("abort", abort, { once: true });
    if (request.signal?.aborted) { abort(); throw abortError(); }
    try {
      const info = await host.headers(id);
      if (closed) throw abortError();
      const body = new ReadableStream({
        start(controller) { streamController = controller; },
        async pull(controller) {
          try {
            const chunk = await host.read(id);
            if (closed) return;
            if (chunk === null) { close(); controller.close(); }
            else controller.enqueue(chunk);
          } catch (error) {
            close();
            controller.error(request.signal?.aborted ? abortError() : error);
          }
        },
        cancel() { close(); },
      }, { highWaterMark: 0 });
      return new Response(body, info);
    } catch (error) {
      close();
      throw request.signal?.aborted ? abortError() : error;
    }
  }
  async function fetch(input, init) {
    const request = new Request(input, init);
    let url = new URL(request.url);
    for (let redirects = 0;; redirects++) {
      const response = await send(request, url);
      response.redirected = redirects > 0;
      const location = response.headers.get("location");
      if (![301, 302, 303, 307, 308].includes(response.status) || !location || request.redirect === "manual") return response;
      await response.body.cancel();
      if (request.redirect === "error" || redirects >= 20) throw new TypeError("HTTP redirect rejected");
      const next = new URL(location, url);
      if (next.origin !== url.origin) {
        for (const name of ["authorization", "proxy-authorization", "cookie", "api-key", "x-api-key"]) request.headers.delete(name);
      }
      if (response.status === 303 && request.method !== "HEAD" || [301, 302].includes(response.status) && request.method === "POST") {
        request.method = "GET";
        request.body = undefined;
        for (const name of ["content-type", "content-length"]) request.headers.delete(name);
      }
      url = next;
    }
  }
  return { fetch, Headers, Request, Response, URL, URLSearchParams, AbortController, AbortSignal, ReadableStream, FormData };
}
