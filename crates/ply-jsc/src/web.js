// The Web platform objects plugins get as globals (RFC 0013): fetch and
// what it needs. Written for the runtime, not polyfilled: URL parsing is the
// Rust `url` crate, sockets and TLS are Rust HTTP, and a stream pull reads
// one native chunk. Evaluated once per realm with the prelude's host calls.
(function (op, opAsync, plain) {
  "use strict";
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  const ABORT = Symbol("abort");
  const KEY = Symbol("construct");
  const STOP = Symbol("stop");
  const SEARCH = Symbol("search");
  const SET_BODY = Symbol("setBody");

  // ------------------------------------------------------------ events
  class DOMException extends Error {
    constructor(message = "", name = "Error") {
      super(message);
      Object.defineProperty(this, "name", { value: String(name), configurable: true, writable: true });
    }
    get code() { return { AbortError: 20, TimeoutError: 23, NotSupportedError: 9, InvalidStateError: 11 }[this.name] ?? 0; }
  }

  class Event {
    constructor(type, init = {}) {
      this.type = String(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = !!init.cancelable;
      this.defaultPrevented = false;
      this.target = null;
      this.currentTarget = null;
      this.timeStamp = Date.now();
    }
    preventDefault() { if (this.cancelable) this.defaultPrevented = true; }
    stopPropagation() {}
    stopImmediatePropagation() { this[STOP] = true; }
  }

  class EventTarget {
    #listeners = new Map();
    addEventListener(type, listener, options) {
      if (listener == null) return;
      const once = typeof options === "object" && !!options?.once;
      const signal = typeof options === "object" ? options?.signal : undefined;
      if (signal?.aborted) return;
      const list = this.#listeners.get(type) ?? [];
      if (list.some(entry => entry.listener === listener)) return;
      list.push({ listener, once });
      this.#listeners.set(type, list);
      signal?.addEventListener("abort", () => this.removeEventListener(type, listener), { once: true });
    }
    removeEventListener(type, listener) {
      const list = this.#listeners.get(type);
      const index = list ? list.findIndex(entry => entry.listener === listener) : -1;
      if (index >= 0) list.splice(index, 1);
    }
    dispatchEvent(event) {
      event.target = this;
      event.currentTarget = this;
      const handler = this["on" + event.type];
      const entries = [...(this.#listeners.get(event.type) ?? [])];
      if (typeof handler === "function") entries.unshift({ listener: handler, once: false });
      for (const { listener, once } of entries) {
        if (once) this.removeEventListener(event.type, listener);
        try {
          if (typeof listener === "function") listener.call(this, event);
          else listener.handleEvent(event);
        } catch (error) {
          // A throwing listener does not stop the others, as in browsers.
          queueMicrotask(() => { throw error; });
        }
        if (event[STOP]) break;
      }
      event.currentTarget = null;
      return !event.defaultPrevented;
    }
  }

  // ------------------------------------------------------------- abort
  class AbortSignal extends EventTarget {
    #aborted = false;
    #reason = undefined;
    constructor(key) {
      if (key !== KEY) throw new TypeError("Illegal constructor");
      super();
      this.onabort = null;
    }
    get aborted() { return this.#aborted; }
    get reason() { return this.#reason; }
    throwIfAborted() { if (this.#aborted) throw this.#reason; }
    [ABORT](reason) {
      if (this.#aborted) return;
      this.#aborted = true;
      this.#reason = reason === undefined ? new DOMException("This operation was aborted", "AbortError") : reason;
      this.dispatchEvent(new Event("abort"));
    }
    static abort(reason) {
      const signal = new AbortSignal(KEY);
      signal[ABORT](reason);
      return signal;
    }
    static timeout(ms) {
      const signal = new AbortSignal(KEY);
      setTimeout(() => signal[ABORT](new DOMException("The operation was aborted due to timeout", "TimeoutError")), ms);
      return signal;
    }
    static any(signals) {
      const signal = new AbortSignal(KEY);
      const aborted = [...signals].find(s => s.aborted);
      if (aborted) signal[ABORT](aborted.reason);
      else for (const s of signals) s.addEventListener("abort", () => signal[ABORT](s.reason), { once: true });
      return signal;
    }
  }

  class AbortController {
    #signal = new AbortSignal(KEY);
    get signal() { return this.#signal; }
    abort(reason) { this.#signal[ABORT](reason); }
  }

  // -------------------------------------------------------------- URLs
  const invalidUrl = () => Object.assign(new TypeError("Invalid URL"), { code: "ERR_INVALID_URL" });
  const urlOp = (name, args) => {
    try { return op(name, args); } catch (error) { throw error?.code === "ERR_INVALID_URL" ? invalidUrl() : error; }
  };

  class URLSearchParams {
    #list = [];
    #url = null;
    constructor(init = "") {
      if (init instanceof URLSearchParams) this.#list = init.#list.map(([k, v]) => [k, v]);
      else if (init !== null && typeof init === "object" && typeof init[Symbol.iterator] === "function") {
        for (const pair of init) {
          const entry = [...pair];
          if (entry.length !== 2) throw new TypeError("Each query pair must be a [name, value] tuple");
          this.#list.push([String(entry[0]), String(entry[1])]);
        }
      } else if (init !== null && typeof init === "object") {
        for (const key of Object.keys(init)) this.#list.push([key, String(init[key])]);
      } else this.#parse(String(init));
    }
    #parse(query) {
      if (query.startsWith("?")) query = query.slice(1);
      this.#list = query ? op("url.form_parse", { input: query }) : [];
    }
    #update() { this.#url?.[SEARCH](this.toString()); }
    // The URL this object reflects, and its query when the URL changes.
    static [SEARCH](params, url, query) { params.#url = url; params.#parse(query); }
    get size() { return this.#list.length; }
    append(name, value) { this.#list.push([String(name), String(value)]); this.#update(); }
    delete(name, value) {
      name = String(name);
      this.#list = this.#list.filter(([k, v]) => k !== name || (value !== undefined && v !== String(value)));
      this.#update();
    }
    get(name) { return this.#list.find(([k]) => k === String(name))?.[1] ?? null; }
    getAll(name) { return this.#list.filter(([k]) => k === String(name)).map(([, v]) => v); }
    has(name, value) { return this.#list.some(([k, v]) => k === String(name) && (value === undefined || v === String(value))); }
    set(name, value) {
      name = String(name);
      const index = this.#list.findIndex(([k]) => k === name);
      if (index < 0) this.#list.push([name, String(value)]);
      else this.#list = this.#list.filter(([k], i) => k !== name || i === index).map((e, i) => (i === index ? [name, String(value)] : e));
      this.#update();
    }
    sort() { this.#list.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)); this.#update(); }
    forEach(fn, thisArg) { for (const [k, v] of this.#list) fn.call(thisArg, v, k, this); }
    *entries() { for (const [k, v] of this.#list) yield [k, v]; }
    *keys() { for (const [k] of this.#list) yield k; }
    *values() { for (const [, v] of this.#list) yield v; }
    [Symbol.iterator]() { return this.entries(); }
    toString() { return op("url.form_serialize", { pairs: this.#list }); }
  }

  class URL {
    #parts;
    #params = null;
    constructor(input, base) {
      const args = { input: String(input) };
      if (base !== undefined) args.base = String(base);
      this.#parts = urlOp("url.parse", args);
    }
    static canParse(input, base) { try { new URL(input, base); return true; } catch { return false; } }
    static parse(input, base) { try { return new URL(input, base); } catch { return null; } }
    #set(part, value) {
      this.#parts = urlOp("url.set", { href: this.#parts.href, part, value: String(value) });
      if (this.#params && (part === "href" || part === "search")) URLSearchParams[SEARCH](this.#params, this, this.#parts.search);
    }
    [SEARCH](query) { this.#parts = urlOp("url.set", { href: this.#parts.href, part: "search", value: query }); }
    get href() { return this.#parts.href; }
    set href(value) { this.#set("href", value); }
    get origin() { return this.#parts.origin; }
    get protocol() { return this.#parts.protocol; }
    set protocol(value) { this.#set("protocol", value); }
    get username() { return this.#parts.username; }
    set username(value) { this.#set("username", value); }
    get password() { return this.#parts.password; }
    set password(value) { this.#set("password", value); }
    get host() { return this.#parts.host; }
    set host(value) { this.#set("host", value); }
    get hostname() { return this.#parts.hostname; }
    set hostname(value) { this.#set("hostname", value); }
    get port() { return this.#parts.port; }
    set port(value) { this.#set("port", value); }
    get pathname() { return this.#parts.pathname; }
    set pathname(value) { this.#set("pathname", value); }
    get search() { return this.#parts.search; }
    set search(value) { this.#set("search", value); }
    get hash() { return this.#parts.hash; }
    set hash(value) { this.#set("hash", value); }
    get searchParams() {
      if (!this.#params) {
        this.#params = new URLSearchParams();
        URLSearchParams[SEARCH](this.#params, this, this.#parts.search);
      }
      return this.#params;
    }
    toString() { return this.#parts.href; }
    toJSON() { return this.#parts.href; }
  }

  // ----------------------------------------------------------- headers
  const TOKEN = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
  const headerName = (name) => {
    name = String(name);
    if (!TOKEN.test(name)) throw new TypeError(`Invalid header name: "${name}"`);
    return name.toLowerCase();
  };
  const headerValue = (value) => {
    value = String(value).replace(/^[\t\n\r ]+|[\t\n\r ]+$/g, "");
    if (/[\0\r\n]/.test(value)) throw new TypeError("Invalid header value");
    return value;
  };

  class Headers {
    #map = new Map();
    constructor(init) {
      if (init == null) return;
      if (init instanceof Headers) for (const [name, value] of init) this.append(name, value);
      else if (typeof init[Symbol.iterator] === "function") {
        for (const pair of init) {
          const entry = [...pair];
          if (entry.length !== 2) throw new TypeError("Header pairs must have a name and a value");
          this.append(entry[0], entry[1]);
        }
      } else if (typeof init === "object") for (const name of Object.keys(init)) this.append(name, init[name]);
      else throw new TypeError("Invalid headers");
    }
    append(name, value) {
      name = headerName(name);
      const values = this.#map.get(name);
      if (values) values.push(headerValue(value));
      else this.#map.set(name, [headerValue(value)]);
    }
    delete(name) { this.#map.delete(headerName(name)); }
    get(name) { return this.#map.get(headerName(name))?.join(", ") ?? null; }
    getSetCookie() { return [...(this.#map.get("set-cookie") ?? [])]; }
    has(name) { return this.#map.has(headerName(name)); }
    set(name, value) { this.#map.set(headerName(name), [headerValue(value)]); }
    forEach(fn, thisArg) { for (const [name, value] of this) fn.call(thisArg, value, name, this); }
    *entries() {
      for (const name of [...this.#map.keys()].sort()) {
        if (name === "set-cookie") for (const value of this.#map.get(name)) yield [name, value];
        else yield [name, this.#map.get(name).join(", ")];
      }
    }
    *keys() { for (const [name] of this) yield name; }
    *values() { for (const [, value] of this) yield value; }
    [Symbol.iterator]() { return this.entries(); }
  }

  // ----------------------------------------------------------- streams
  class ReadableStreamDefaultReader {
    #stream;
    #closed;
    constructor(stream) {
      if (stream.locked) throw new TypeError("ReadableStream is locked");
      this.#stream = stream;
      this.#closed = stream[KEY].lock(this);
    }
    get closed() { return this.#closed; }
    read() {
      if (!this.#stream) return Promise.reject(new TypeError("Reader is released"));
      return this.#stream[KEY].read();
    }
    releaseLock() {
      if (!this.#stream) return;
      this.#stream[KEY].unlock();
      this.#stream = null;
    }
    cancel(reason) {
      if (!this.#stream) return Promise.reject(new TypeError("Reader is released"));
      return this.#stream.cancel(reason, KEY);
    }
  }

  class ReadableStream {
    #queue = [];
    #reads = [];
    #state = "readable";
    #error = undefined;
    #reader = null;
    #closing = false;
    #started = false;
    #pulling = false;
    #pullAgain = false;
    #source;
    #highWaterMark;
    #controller;
    #closedHandlers = null;
    constructor(source = {}, strategy = {}) {
      if (source?.type === "bytes") throw new TypeError("byte streams are not supported");
      this.#source = source ?? {};
      this.#highWaterMark = strategy?.highWaterMark ?? 1;
      const stream = this;
      this.#controller = {
        get desiredSize() { return stream.#state === "readable" ? stream.#highWaterMark - stream.#queue.length : stream.#state === "closed" ? 0 : null; },
        enqueue(chunk) { stream.#enqueue(chunk); },
        close() { stream.#requestClose(); },
        error(error) { stream.#fail(error); },
      };
      Object.defineProperty(this, KEY, { value: {
        lock: (reader) => {
          this.#reader = reader;
          const closed = new Promise((resolve, reject) => {
            if (this.#state === "closed") resolve();
            else if (this.#state === "errored") reject(this.#error);
            else this.#closedHandlers = { resolve, reject };
          });
          // Reads report the error; an unobserved `closed` must not fail the call.
          closed.catch(() => {});
          return closed;
        },
        unlock: () => {
          for (const read of this.#reads.splice(0)) read.reject(new TypeError("Reader was released"));
          this.#closedHandlers = null;
          this.#reader = null;
        },
        read: () => this.#read(),
      } });
      let started;
      try { started = this.#source.start?.(this.#controller); } catch (error) { this.#fail(error); }
      Promise.resolve(started).then(() => { this.#started = true; this.#pull(); }, (error) => this.#fail(error));
    }
    get locked() { return this.#reader !== null; }
    getReader(options) {
      if (options?.mode === "byob") throw new TypeError("BYOB readers are not supported");
      return new ReadableStreamDefaultReader(this);
    }
    cancel(reason, key) {
      if (this.locked && key !== KEY) return Promise.reject(new TypeError("ReadableStream is locked"));
      if (this.#state === "closed") return Promise.resolve();
      if (this.#state === "errored") return Promise.reject(this.#error);
      this.#queue = [];
      this.#finish();
      return Promise.resolve(this.#source.cancel?.(reason)).then(() => undefined);
    }
    #read() {
      if (this.#queue.length) {
        const value = this.#queue.shift();
        if (this.#closing && !this.#queue.length) this.#finish();
        else this.#pull();
        return Promise.resolve({ value, done: false });
      }
      if (this.#state === "closed") return Promise.resolve({ value: undefined, done: true });
      if (this.#state === "errored") return Promise.reject(this.#error);
      const read = new Promise((resolve, reject) => this.#reads.push({ resolve, reject }));
      this.#pull();
      return read;
    }
    #enqueue(chunk) {
      if (this.#state !== "readable" || this.#closing) throw new TypeError("The stream is not readable");
      if (this.#reads.length) this.#reads.shift().resolve({ value: chunk, done: false });
      else this.#queue.push(chunk);
      this.#pull();
    }
    #requestClose() {
      if (this.#state !== "readable" || this.#closing) throw new TypeError("The stream is not readable");
      this.#closing = true;
      if (!this.#queue.length) this.#finish();
    }
    #finish() {
      this.#state = "closed";
      for (const read of this.#reads.splice(0)) read.resolve({ value: undefined, done: true });
      this.#closedHandlers?.resolve();
    }
    #fail(error) {
      if (this.#state !== "readable") return;
      this.#state = "errored";
      this.#error = error;
      this.#queue = [];
      for (const read of this.#reads.splice(0)) read.reject(error);
      this.#closedHandlers?.reject(error);
    }
    #pull() {
      if (!this.#started || this.#state !== "readable" || this.#closing || typeof this.#source.pull !== "function") return;
      if (this.#highWaterMark - this.#queue.length <= 0 && !this.#reads.length) return;
      if (this.#pulling) { this.#pullAgain = true; return; }
      this.#pulling = true;
      let pulled;
      try { pulled = this.#source.pull(this.#controller); } catch (error) { this.#fail(error); return; }
      Promise.resolve(pulled).then(() => {
        this.#pulling = false;
        if (this.#pullAgain) { this.#pullAgain = false; this.#pull(); }
      }, (error) => this.#fail(error));
    }
    tee() {
      const reader = this.getReader();
      const branches = [];
      let reading = null;
      let cancelled = 0;
      const pull = () => reading ??= reader.read().then(({ value, done }) => {
        reading = null;
        for (const branch of branches) {
          if (branch.cancelled) continue;
          if (done) branch.controller.close();
          else branch.controller.enqueue(value);
        }
      }, (error) => { for (const branch of branches) if (!branch.cancelled) branch.controller.error(error); });
      const branch = (index) => new ReadableStream({
        start(controller) { branches[index] = { controller, cancelled: false }; },
        pull,
        cancel(reason) {
          branches[index].cancelled = true;
          if (++cancelled === 2) return reader.cancel(reason);
        },
      });
      return [branch(0), branch(1)];
    }
    values({ preventCancel = false } = {}) {
      const reader = this.getReader();
      let finished = false;
      return {
        next: () => {
          if (finished) return Promise.resolve({ value: undefined, done: true });
          return reader.read().then((result) => {
            if (result.done) { finished = true; reader.releaseLock(); }
            return result;
          }, (error) => { finished = true; reader.releaseLock(); throw error; });
        },
        return: (value) => {
          if (finished) return Promise.resolve({ value, done: true });
          finished = true;
          if (preventCancel) { reader.releaseLock(); return Promise.resolve({ value, done: true }); }
          return reader.cancel(value).then(() => { reader.releaseLock(); return { value, done: true }; });
        },
        [Symbol.asyncIterator]() { return this; },
      };
    }
    [Symbol.asyncIterator](options) { return this.values(options); }
    static from(iterable) {
      const iterator = typeof iterable[Symbol.asyncIterator] === "function" ? iterable[Symbol.asyncIterator]() : iterable[Symbol.iterator]();
      return new ReadableStream({
        pull: (controller) => Promise.resolve(iterator.next()).then(({ value, done }) => (done ? controller.close() : controller.enqueue(value))),
        cancel: (reason) => iterator.return?.(reason),
      }, { highWaterMark: 0 });
    }
  }

  // -------------------------------------------------------------- bodies
  // SDKs check `instanceof FormData` even for JSON requests. It keeps string
  // entries; multipart bodies are not supported.
  class FormData {
    #entries = [];
    append(name, value) { this.#entries.push([String(name), String(value)]); }
    delete(name) { this.#entries = this.#entries.filter(([key]) => key !== String(name)); }
    get(name) { return this.#entries.find(([key]) => key === String(name))?.[1] ?? null; }
    getAll(name) { return this.#entries.filter(([key]) => key === String(name)).map(([, value]) => value); }
    has(name) { return this.#entries.some(([key]) => key === String(name)); }
    set(name, value) { this.delete(name); this.append(name, value); }
    forEach(fn, thisArg) { for (const [key, value] of this.#entries) fn.call(thisArg, value, key, this); }
    *entries() { yield* this.#entries.map(([k, v]) => [k, v]); }
    *keys() { for (const [key] of this.#entries) yield key; }
    *values() { for (const [, value] of this.#entries) yield value; }
    [Symbol.iterator]() { return this.entries(); }
  }

  const bytes = (view) => new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
  // A body init as bytes or a stream, with the content type it implies.
  function extract(body) {
    if (body == null) return { body: null, type: null };
    if (body instanceof ReadableStream) return { body, type: null };
    if (typeof body === "string") return { body: encoder.encode(body), type: "text/plain;charset=UTF-8" };
    if (body instanceof URLSearchParams) return { body: encoder.encode(body.toString()), type: "application/x-www-form-urlencoded;charset=UTF-8" };
    if (body instanceof ArrayBuffer) return { body: new Uint8Array(body.slice(0)), type: null };
    if (ArrayBuffer.isView(body)) return { body: bytes(body).slice(), type: null };
    if (body instanceof FormData) throw new TypeError("multipart form bodies are not supported");
    return { body: encoder.encode(String(body)), type: "text/plain;charset=UTF-8" };
  }
  const streamOf = (data) => new ReadableStream({ start(controller) { if (data.byteLength) controller.enqueue(data); controller.close(); } });
  async function collect(stream) {
    const reader = stream.getReader();
    const chunks = [];
    let length = 0;
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        const chunk = typeof value === "string" ? encoder.encode(value) : ArrayBuffer.isView(value) ? bytes(value) : new Uint8Array(value);
        chunks.push(chunk);
        length += chunk.byteLength;
      }
    } finally { reader.releaseLock(); }
    const out = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) { out.set(chunk, offset); offset += chunk.byteLength; }
    return out;
  }

  // Shared by Request and Response: the body is read at most once.
  class Body {
    #body;
    #used = false;
    constructor(body) { this.#body = body; }
    get body() {
      if (this.#body instanceof Uint8Array) this.#body = streamOf(this.#body);
      return this.#body;
    }
    get bodyUsed() { return this.#used; }
    async bytes() {
      if (this.#used || (this.#body instanceof ReadableStream && this.#body.locked)) throw new TypeError("Body is unusable: it has already been read");
      this.#used = true;
      if (this.#body == null) return new Uint8Array(0);
      return this.#body instanceof Uint8Array ? this.#body.slice() : await collect(this.#body);
    }
    async arrayBuffer() { return (await this.bytes()).buffer; }
    async text() { return decoder.decode(await this.bytes()); }
    async json() { return JSON.parse(await this.text()); }
    async formData() { throw new TypeError("form data bodies are not supported"); }
    [SET_BODY](body) { this.#body = body; }
    // The body, split for a clone; this object keeps one branch.
    [KEY]() {
      if (this.#used) throw new TypeError("Body is unusable: it has already been read");
      if (!(this.#body instanceof ReadableStream)) return this.#body;
      const [mine, theirs] = this.#body.tee();
      this.#body = mine;
      return theirs;
    }
  }

  const METHODS = ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT", "PATCH"];
  class Request extends Body {
    #signal;
    constructor(input, init = {}) {
      const previous = input instanceof Request ? input : null;
      const source = init.body !== undefined ? extract(init.body) : { body: previous ? previous[KEY]() : null, type: null };
      super(source.body);
      this.url = previous ? previous.url : new URL(String(input)).href;
      const method = String(init.method ?? previous?.method ?? "GET");
      this.method = METHODS.includes(method.toUpperCase()) ? method.toUpperCase() : method;
      this.headers = new Headers(init.headers ?? previous?.headers);
      if (source.type && !this.headers.has("content-type")) this.headers.set("content-type", source.type);
      this.redirect = init.redirect ?? previous?.redirect ?? "follow";
      if (!["follow", "manual", "error"].includes(this.redirect)) throw new TypeError("Invalid redirect mode");
      this.#signal = init.signal ?? previous?.signal ?? new AbortController().signal;
      if ((this.method === "GET" || this.method === "HEAD") && source.body != null) throw new TypeError("Request with GET/HEAD method cannot have body");
    }
    get signal() { return this.#signal; }
    clone() { return new Request(this); }
  }

  class Response extends Body {
    constructor(body = null, init = {}) {
      const source = extract(body);
      super(source.body);
      this.status = init.status ?? 200;
      if (!Number.isInteger(this.status) || this.status < 200 || this.status > 599) throw new RangeError("Invalid response status");
      this.statusText = String(init.statusText ?? "");
      this.headers = new Headers(init.headers);
      if (source.type && !this.headers.has("content-type")) this.headers.set("content-type", source.type);
      this.url = "";
      this.redirected = false;
      this.type = "default";
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    clone() {
      const theirs = this[KEY]();
      const clone = new Response(null, this);
      clone[SET_BODY](theirs instanceof Uint8Array ? theirs.slice() : theirs);
      clone.url = this.url;
      clone.redirected = this.redirected;
      clone.type = this.type;
      return clone;
    }
    static json(data, init = {}) {
      const headers = new Headers(init.headers);
      if (!headers.has("content-type")) headers.set("content-type", "application/json");
      return new Response(JSON.stringify(data), { ...init, headers });
    }
    static error() {
      const response = new Response(null, { status: 200 });
      Object.assign(response, { type: "error", status: 0 });
      return response;
    }
    static redirect(url, status = 302) {
      if (![301, 302, 303, 307, 308].includes(status)) throw new RangeError("Invalid redirect status");
      return new Response(null, { status, headers: { location: new URL(url).href } });
    }
  }

  // --------------------------------------------------------------- fetch
  // A cancelled call's AbortError passes through untouched (RFC 0012); other
  // network failures are Node's `TypeError: fetch failed`.
  const cancelled = (error) => error?.code === "ERR_PLY_CANCELLED";
  const failed = (error) => (cancelled(error) ? error : new TypeError("fetch failed", { cause: error }));

  async function send(request, url, method, headers, body) {
    const signal = request.signal;
    signal.throwIfAborted();
    let id;
    try {
      id = op("http.open", { url: url.href, method, headers: [...headers] }, body && plain(body));
    } catch (error) {
      throw failed(error);
    }
    let open = true;
    let controller = null;
    const close = () => {
      if (!open) return;
      open = false;
      signal.removeEventListener("abort", onAbort);
      op("http.abort", { id });
    };
    const onAbort = () => { close(); controller?.error(signal.reason); };
    signal.addEventListener("abort", onAbort, { once: true });
    let info;
    try {
      info = await opAsync("http.headers", { id });
    } catch (error) {
      close();
      throw signal.aborted ? signal.reason : failed(error);
    }
    if (!open) throw signal.reason;
    const stream = new ReadableStream({
      start(c) { controller = c; },
      async pull(c) {
        let chunk;
        try {
          chunk = await opAsync("http.read", { id });
        } catch (error) {
          close();
          c.error(signal.aborted ? signal.reason : cancelled(error) ? error : new TypeError("terminated", { cause: error }));
          return;
        }
        if (!open) return;
        if (chunk === null) { close(); c.close(); } else c.enqueue(chunk);
      },
      cancel() { close(); },
    }, { highWaterMark: 0 });
    const response = new Response(stream, { status: info.status, statusText: info.statusText, headers: info.headers });
    response.url = info.url;
    return response;
  }

  async function fetch(input, init = undefined) {
    const request = new Request(input, init);
    let body = null;
    try {
      body = request.body ? await request.bytes() : null;
    } catch (error) { throw failed(error); }
    let url = new URL(request.url);
    let method = request.method;
    const headers = new Headers(request.headers);
    for (let redirects = 0; ; redirects++) {
      const response = await send(request, url, method, headers, body);
      response.redirected = redirects > 0;
      const location = response.headers.get("location");
      if (![301, 302, 303, 307, 308].includes(response.status) || !location || request.redirect === "manual") return response;
      await response.body.cancel();
      if (request.redirect === "error" || redirects >= 20) throw new TypeError("fetch failed", { cause: new Error("redirect rejected") });
      const next = new URL(location, url);
      // Credentials never follow a redirect to another origin.
      if (next.origin !== url.origin) for (const name of ["authorization", "proxy-authorization", "cookie", "api-key", "x-api-key"]) headers.delete(name);
      if ((response.status === 303 && method !== "HEAD") || ([301, 302].includes(response.status) && method === "POST")) {
        method = "GET";
        body = null;
        for (const name of ["content-type", "content-length"]) headers.delete(name);
      }
      url = next;
    }
  }

  return {
    fetch, Headers, Request, Response, URL, URLSearchParams, AbortController, AbortSignal,
    ReadableStream, ReadableStreamDefaultReader, FormData, Event, EventTarget, DOMException,
  };
})
