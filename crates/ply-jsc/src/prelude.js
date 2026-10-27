// ply realm prelude: a node-aligned environment built on ply host ops.
//
// Evaluated once per realm. The host functions arrive as parameters and are
// never put on the global object, so plugin code can only reach the outside
// world through the modules defined here (ply-io and the model HTTP host).
//
// This file is not a security boundary: plugin code runs in the same realm
// and can replace globals. The Rust side validates every host call. The
// built-ins captured below only keep the runtime working when plugin code
// monkeypatches JSON, eval or Object.
(function (host, hostAsync, configJson, asyncHelpers) {
  "use strict";
  const jsonParse = JSON.parse;
  const jsonStringify = JSON.stringify;
  const indirectEval = eval;
  const { create: objectCreate, assign: objectAssign, entries: objectEntries, freeze: objectFreeze, hasOwn: objectHasOwn } = Object;
  const config = jsonParse(configJson);
  const G = globalThis;
  // Only generated code sees the lexical helper binding. Module globals
  // cannot replace it, and the native callback stores identity privately.
  const evaluate = indirectEval("(function(eval, babelHelpers, code) { return eval(code); })");
  const compile = code => evaluate(indirectEval, asyncHelpers, code);
  const EXIT = Symbol("ply.exit");

  // ---------------------------------------------------------------- host ops
  function makeError(e) {
    const err = new Error(e.message);
    for (const k of ["name", "code", "errno", "syscall", "path", "dest"]) if (e[k] !== undefined && e[k] !== null) err[k] = e[k];
    return err;
  }
  function unwrap(r) {
    if (r !== null && typeof r === "object" && !(r instanceof Uint8Array) && r.__error) {
      if (r.__error.code === "ERR_PLY_CANCELLED") asyncHelpers.observeCancel();
      throw makeError(r.__error);
    }
    return r;
  }
  // Null-prototype copies: a plugin-defined Object.prototype.toJSON cannot
  // rewrite what is sent (the host checks it regardless).
  const callArgs = (args) => jsonStringify(args === undefined ? null : objectAssign(objectCreate(null), args));
  const op = (name, args, bytes) => unwrap(host(name, callArgs(args), bytes));
  const opAsync = async (name, args, bytes) => unwrap(await hostAsync(name, callArgs(args), bytes));
  const plain = (u8) =>
    u8.byteOffset === 0 && u8.byteLength === u8.buffer.byteLength && Object.getPrototypeOf(u8) === Uint8Array.prototype
      ? u8
      : new Uint8Array(u8);

  // ------------------------------------------------------------- encodings
  function utf8Encode(str) {
    const out = [];
    for (let i = 0; i < str.length; i++) {
      let c = str.charCodeAt(i);
      if (c >= 0xd800 && c <= 0xdbff && i + 1 < str.length) {
        const d = str.charCodeAt(i + 1);
        if (d >= 0xdc00 && d <= 0xdfff) {
          c = 0x10000 + ((c - 0xd800) << 10) + (d - 0xdc00);
          i++;
        } else c = 0xfffd;
      } else if (c >= 0xd800 && c <= 0xdfff) c = 0xfffd;
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
      else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
      else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    }
    return new Uint8Array(out);
  }
  function utf8Decode(bytes) {
    const cps = [];
    let i = 0;
    while (i < bytes.length) {
      const b = bytes[i];
      let cp, n;
      if (b < 0x80) { cp = b; n = 1; }
      else if ((b & 0xe0) === 0xc0) { cp = b & 31; n = 2; }
      else if ((b & 0xf0) === 0xe0) { cp = b & 15; n = 3; }
      else if ((b & 0xf8) === 0xf0) { cp = b & 7; n = 4; }
      else { cps.push(0xfffd); i++; continue; }
      if (i + n > bytes.length) { cps.push(0xfffd); break; }
      let ok = true;
      for (let j = 1; j < n; j++) {
        const c = bytes[i + j];
        if ((c & 0xc0) !== 0x80) { ok = false; n = j; break; }
        cp = (cp << 6) | (c & 63);
      }
      cps.push(ok ? cp : 0xfffd);
      i += n;
    }
    let s = "";
    for (let k = 0; k < cps.length; k += 8192) s += String.fromCodePoint(...cps.slice(k, k + 8192));
    return s;
  }
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  function base64Encode(bytes) {
    let s = "";
    for (let i = 0; i < bytes.length; i += 3) {
      const n = (bytes[i] << 16) | ((bytes[i + 1] ?? 0) << 8) | (bytes[i + 2] ?? 0);
      s += B64[(n >> 18) & 63] + B64[(n >> 12) & 63];
      s += i + 1 < bytes.length ? B64[(n >> 6) & 63] : "=";
      s += i + 2 < bytes.length ? B64[n & 63] : "=";
    }
    return s;
  }
  function base64Decode(str) {
    const clean = str.replace(/[^A-Za-z0-9+/_-]/g, "").replace(/-/g, "+").replace(/_/g, "/");
    const out = [];
    for (let i = 0; i < clean.length; i += 4) {
      const n = (B64.indexOf(clean[i]) << 18) | (B64.indexOf(clean[i + 1]) << 12) |
        ((clean[i + 2] ? B64.indexOf(clean[i + 2]) : 0) << 6) | (clean[i + 3] ? B64.indexOf(clean[i + 3]) : 0);
      out.push((n >> 16) & 255);
      if (clean[i + 2]) out.push((n >> 8) & 255);
      if (clean[i + 3]) out.push(n & 255);
    }
    return new Uint8Array(out);
  }
  function encode(str, enc = "utf8") {
    switch (String(enc).toLowerCase()) {
      case "utf8": case "utf-8": return utf8Encode(str);
      case "base64": case "base64url": return base64Decode(str);
      case "hex": {
        const out = new Uint8Array(str.length >> 1);
        for (let i = 0; i < out.length; i++) out[i] = parseInt(str.substr(i * 2, 2), 16);
        return out;
      }
      case "latin1": case "binary": case "ascii": return Uint8Array.from(str, (c) => c.charCodeAt(0) & 255);
      default: throw new TypeError(`Unknown encoding: ${enc}`);
    }
  }
  function decode(bytes, enc = "utf8") {
    switch (String(enc).toLowerCase()) {
      case "utf8": case "utf-8": return utf8Decode(bytes);
      case "base64": return base64Encode(bytes);
      case "base64url": return base64Encode(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
      case "hex": return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
      case "latin1": case "binary": case "ascii": return String.fromCharCode(...bytes);
      default: throw new TypeError(`Unknown encoding: ${enc}`);
    }
  }

  class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(s = "") { return utf8Encode(String(s)); }
  }
  class TextDecoder {
    get encoding() { return "utf-8"; }
    decode(b) { return b ? utf8Decode(b instanceof Uint8Array ? b : new Uint8Array(b.buffer ?? b)) : ""; }
  }

  class Buffer extends Uint8Array {
    static from(v, enc) {
      if (typeof v === "string") return toBuffer(encode(v, enc));
      if (v instanceof ArrayBuffer) return toBuffer(new Uint8Array(v.slice(0)));
      if (ArrayBuffer.isView(v)) return toBuffer(new Uint8Array(v.buffer.slice(v.byteOffset, v.byteOffset + v.byteLength)));
      if (Array.isArray(v)) return toBuffer(new Uint8Array(v));
      if (v && v.type === "Buffer" && Array.isArray(v.data)) return toBuffer(new Uint8Array(v.data));
      throw new TypeError("The first argument must be a string, Buffer, ArrayBuffer, Array, or array-like object.");
    }
    static alloc(n, fill = 0) {
      const b = toBuffer(new Uint8Array(n));
      if (fill) b.fill(typeof fill === "string" ? fill.charCodeAt(0) : fill);
      return b;
    }
    static allocUnsafe(n) { return Buffer.alloc(n); }
    static isBuffer(b) { return b instanceof Buffer; }
    static byteLength(s, enc) { return typeof s === "string" ? encode(s, enc).length : s.byteLength; }
    static concat(list, total) {
      const len = total ?? list.reduce((n, b) => n + b.length, 0);
      const out = Buffer.alloc(len);
      let at = 0;
      for (const b of list) {
        out.set(b.subarray(0, Math.max(0, len - at)), at);
        at += b.length;
        if (at >= len) break;
      }
      return out;
    }
    toString(enc = "utf8", start = 0, end = this.length) { return decode(this.subarray(start, end), enc); }
    toJSON() { return { type: "Buffer", data: Array.from(this) }; }
    equals(o) { return this.length === o.length && this.every((b, i) => b === o[i]); }
  }
  function toBuffer(u8) {
    Object.setPrototypeOf(u8, Buffer.prototype);
    return u8;
  }

  // --------------------------------------------------------------- inspect
  const isIdent = (k) => /^[A-Za-z_$][\w$]*$/.test(k);
  function quote(s) {
    if (s.includes("'") && !s.includes('"')) return `"${s}"`;
    return "'" + s.replace(/\\/g, "\\\\").replace(/'/g, "\\'").replace(/\n/g, "\\n") + "'";
  }
  function inspect(v, opts) { return fmt(v, 0, [], true, (opts && opts.depth) ?? 2); }
  function fmt(v, depth, seen, top, maxDepth) {
    switch (typeof v) {
      case "string": return top ? v : quote(v);
      case "number": return Object.is(v, -0) ? "-0" : String(v);
      case "bigint": return `${v}n`;
      case "boolean": case "undefined": return String(v);
      case "symbol": return v.toString();
      case "function":
        if (/^class\s/.test(Function.prototype.toString.call(v))) return `[class ${v.name || "(anonymous)"}]`;
        return v.name ? `[Function: ${v.name}]` : "[Function (anonymous)]";
    }
    if (v === null) return "null";
    if (seen.includes(v)) return "[Circular *1]";
    if (v instanceof Error) return v.stack ? `${v.name}: ${v.message}\n${v.stack.split("\n").map((l) => "    at " + l).join("\n")}` : `${v.name}: ${v.message}`;
    if (v instanceof Date) return isNaN(v) ? "Invalid Date" : v.toISOString();
    if (v instanceof RegExp) return String(v);
    if (v instanceof Promise) return "Promise { <pending> }";
    if (v instanceof Buffer) return `<Buffer ${Array.from(v.subarray(0, 50), (b) => b.toString(16).padStart(2, "0")).join(" ")}${v.length > 50 ? ` ... ${v.length - 50} more bytes` : ""}>`;
    if (depth > maxDepth) return Array.isArray(v) ? "[Array]" : "[Object]";
    seen.push(v);
    let open, close, items;
    if (Array.isArray(v) || ArrayBuffer.isView(v)) {
      const prefix = Array.isArray(v) ? "" : `${v.constructor.name}(${v.length}) `;
      items = Array.from(v, (x) => fmt(x, depth + 1, seen, false, maxDepth));
      open = prefix + "[";
      close = "]";
    } else if (v instanceof Map) {
      items = Array.from(v, ([k, x]) => `${fmt(k, depth + 1, seen, false, maxDepth)} => ${fmt(x, depth + 1, seen, false, maxDepth)}`);
      open = `Map(${v.size}) {`;
      close = "}";
    } else if (v instanceof Set) {
      items = Array.from(v, (x) => fmt(x, depth + 1, seen, false, maxDepth));
      open = `Set(${v.size}) {`;
      close = "}";
    } else {
      const proto = Object.getPrototypeOf(v);
      const name = proto === null ? "[Object: null prototype]" : proto.constructor && proto.constructor.name;
      items = Object.keys(v).map((k) => `${isIdent(k) ? k : quote(k)}: ${fmt(v[k], depth + 1, seen, false, maxDepth)}`);
      open = (name && name !== "Object" ? name + " " : "") + "{";
      close = "}";
    }
    seen.pop();
    if (items.length === 0) return open.endsWith("[") ? `${open}]` : `${open}}`;
    const oneLine = `${open} ${items.join(", ")} ${close}`;
    if (oneLine.length <= 72 && !items.some((i) => i.includes("\n"))) return oneLine;
    const pad = "  ".repeat(depth + 1);
    return `${open}\n${items.map((i) => pad + i).join(",\n")}\n${"  ".repeat(depth)}${close}`;
  }
  function format(...args) {
    if (typeof args[0] !== "string") return args.map((a) => fmt(a, 0, [], true, 2)).join(" ");
    let i = 1;
    let out = args[0].replace(/%[sdifjoOc%]/g, (m) => {
      if (m === "%%") return "%";
      if (i >= args.length) return m;
      const a = args[i++];
      switch (m) {
        case "%s": return typeof a === "string" ? a : fmt(a, 1, [], false, 1);
        case "%d": case "%i": return String(m === "%i" ? parseInt(a) : Number(a));
        case "%f": return String(parseFloat(a));
        case "%j": try { return jsonStringify(a); } catch { return "[Circular]"; }
        case "%c": return "";
        default: return fmt(a, 0, [], false, 4);
      }
    });
    for (; i < args.length; i++) out += " " + (typeof args[i] === "string" ? args[i] : fmt(args[i], 0, [], true, 2));
    return out;
  }

  // --------------------------------------------------------------- console
  const write = (fd, text) => op("console.write", { fd, text });
  const counters = new Map();
  const timersStart = new Map();
  const console = {
    log: (...a) => write(1, format(...a) + "\n"),
    info: (...a) => write(1, format(...a) + "\n"),
    debug: (...a) => write(1, format(...a) + "\n"),
    warn: (...a) => write(2, format(...a) + "\n"),
    error: (...a) => write(2, format(...a) + "\n"),
    trace: (...a) => write(2, "Trace: " + format(...a) + "\n"),
    dir: (v, o) => write(1, inspect(v, o) + "\n"),
    table: (v) => write(1, inspect(v) + "\n"),
    assert: (ok, ...a) => { if (!ok) write(2, "Assertion failed" + (a.length ? ": " + format(...a) : "") + "\n"); },
    count: (l = "default") => { const n = (counters.get(l) ?? 0) + 1; counters.set(l, n); write(1, `${l}: ${n}\n`); },
    time: (l = "default") => timersStart.set(l, op("clock.now")),
    timeEnd: (l = "default") => { const t = timersStart.get(l); if (t !== undefined) write(1, `${l}: ${(op("clock.now") - t).toFixed(3)}ms\n`); timersStart.delete(l); },
  };

  // ---------------------------------------------------------------- timers
  const timers = new Map();
  class Timeout {
    constructor(id) { this.id = id; }
    ref() { return this; }
    unref() { return this; }
    hasRef() { return true; }
    [Symbol.toPrimitive]() { return this.id; }
  }
  // The host owns scheduling (and re-arms intervals); this side only keeps
  // the callbacks.
  function arm(fn, ms, args, repeat) {
    if (typeof fn !== "function") throw new TypeError('The "callback" argument must be of type function');
    const delay = Math.max(0, Number(ms) || 0);
    const id = op("timer.set", { ms: delay, repeat });
    timers.set(id, { fn: asyncHelpers.bind(fn, true), args, repeat });
    return new Timeout(id);
  }
  const setTimeout = (fn, ms, ...args) => arm(fn, ms, args, false);
  const setInterval = (fn, ms, ...args) => arm(fn, ms, args, true);
  const setImmediate = (fn, ...args) => arm(fn, 0, args, false);
  function clearTimeout(t) {
    const id = t && typeof t === "object" ? t.id : t;
    if (timers.delete(id)) op("timer.clear", { id });
  }
  function reportUncaught(e) {
    write(2, "Uncaught " + (e instanceof Error ? `${e.name}: ${e.message}` : fmt(e, 0, [], false, 2)) + "\n");
  }
  function fire(id) {
    const t = timers.get(id);
    if (!t) return;
    if (!t.repeat) timers.delete(id);
    try {
      t.fn(...t.args);
    } catch (e) {
      if (e && e[EXIT] !== undefined) return;
      reportUncaught(e);
    }
  }
  const queueMicrotask = G.queueMicrotask ?? ((fn) => { Promise.resolve().then(fn); });

  // --------------------------------------------------------------- process
  const env = Object.assign(Object.create(null), config.env);
  const process = {
    env,
    argv: ["ply", ...config.argv],
    argv0: "ply",
    execArgv: [],
    platform: "linux",
    arch: "x64",
    pid: 1,
    ppid: 0,
    title: "ply",
    version: "v22.0.0",
    versions: { node: "22.0.0", ply: config.version },
    release: { name: "node" },
    exitCode: undefined,
    cwd: () => op("process.cwd"),
    chdir: (dir) => { op("process.chdir", { dir: String(dir) }); },
    exit(code) {
      const c = code ?? process.exitCode ?? 0;
      op("process.exit", { code: c });
      const err = new Error(`process.exit(${c})`);
      err[EXIT] = c;
      throw err;
    },
    nextTick: (fn, ...args) => queueMicrotask(() => fn(...args)),
    hrtime: Object.assign((prev) => {
      const ns = BigInt(Math.round(op("clock.now") * 1e6));
      const t = [Number(ns / 1000000000n), Number(ns % 1000000000n)];
      return prev ? [t[0] - prev[0], t[1] - prev[1]] : t;
    }, { bigint: () => BigInt(Math.round(op("clock.now") * 1e6)) }),
    uptime: () => op("clock.now") / 1000,
    memoryUsage: () => ({ rss: 0, heapTotal: 0, heapUsed: 0, external: 0, arrayBuffers: 0 }),
    stdout: { write: (s) => { write(1, String(s)); return true; }, isTTY: false },
    stderr: { write: (s) => { write(2, String(s)); return true; }, isTTY: false },
    on() { return process; },
    once() { return process; },
    off() { return process; },
    emitWarning: (w) => write(2, `Warning: ${w}\n`),
  };

  // ------------------------------------------------------------------ path
  function trimSlash(p) { return p.length > 1 && p.endsWith("/") ? p.slice(0, -1) : p; }
  const path = {
    sep: "/",
    delimiter: ":",
    normalize(p) {
      if (p === "") return ".";
      const n = op("path.normalize", { path: p });
      return p.endsWith("/") && n !== "/" ? n + "/" : n;
    },
    join(...parts) {
      const joined = parts.filter((x) => x !== "").join("/");
      return joined === "" ? "." : path.normalize(joined);
    },
    resolve(...parts) {
      let acc = "";
      for (let i = parts.length - 1; i >= 0 && !acc.startsWith("/"); i--) if (parts[i]) acc = parts[i] + (acc ? "/" + acc : "");
      return trimSlash(op("path.resolve", { cwd: process.cwd(), path: acc || "." }));
    },
    isAbsolute: (p) => p.startsWith("/"),
    dirname: (p) => op("path.dirname", { path: p }),
    basename(p, ext) {
      const b = op("path.basename", { path: p });
      return ext && b.endsWith(ext) && b !== ext ? b.slice(0, -ext.length) : b;
    },
    extname: (p) => op("path.extname", { path: p }),
    relative(from, to) {
      const a = path.resolve(from).split("/").filter(Boolean);
      const b = path.resolve(to).split("/").filter(Boolean);
      let i = 0;
      while (i < a.length && i < b.length && a[i] === b[i]) i++;
      return [...Array(a.length - i).fill(".."), ...b.slice(i)].join("/");
    },
    parse(p) {
      const root = p.startsWith("/") ? "/" : "";
      const base = path.basename(p);
      const ext = path.extname(p);
      const dir = p.includes("/") ? path.dirname(p) : "";
      return { root, dir, base, ext, name: ext ? base.slice(0, -ext.length) : base };
    },
    format: (o) => (o.dir ? o.dir + "/" : o.root ?? "") + (o.base ?? (o.name ?? "") + (o.ext ?? "")),
  };
  path.posix = path;

  // -------------------------------------------------------------------- fs
  const opts = (o) => (typeof o === "string" ? { encoding: o } : o ?? {});
  const toBytes = (data, o) => plain(typeof data === "string" ? encode(data, opts(o).encoding ?? "utf8") : data instanceof Uint8Array ? data : new Uint8Array(data.buffer ?? data));
  class Stats {
    constructor(s) {
      Object.assign(this, { dev: 1, ino: 0, mode: s.isDirectory ? 0o40755 : 0o100644, nlink: 1, uid: 1000, gid: 1000, size: s.size, blksize: 4096, blocks: Math.ceil(s.size / 512) });
      this.mtimeMs = this.atimeMs = this.ctimeMs = this.birthtimeMs = s.mtimeMs;
      this.mtime = this.atime = this.ctime = this.birthtime = new Date(s.mtimeMs);
      Object.defineProperty(this, "_dir", { value: s.isDirectory });
    }
    isFile() { return !this._dir; }
    isDirectory() { return this._dir; }
    isSymbolicLink() { return false; }
    isFIFO() { return false; }
    isSocket() { return false; }
  }
  class Dirent {
    constructor(e, parent) { this.name = e.name; this.parentPath = this.path = parent; Object.defineProperty(this, "_dir", { value: e.isDirectory }); }
    isFile() { return !this._dir; }
    isDirectory() { return this._dir; }
    isSymbolicLink() { return false; }
  }
  // The host returns text for utf-8 reads and bytes otherwise.
  const asResult = (r, o) => {
    if (typeof r === "string") return r;
    const enc = opts(o).encoding;
    return enc ? decode(r, enc) : toBuffer(r);
  };
  function makeFs(call) {
    return {
      readFile: (p, o) => call("fs.readFile", { path: String(p), encoding: opts(o).encoding ?? null }),
      writeFile: (p, d, o) => call("fs.writeFile", { path: String(p), append: opts(o).flag === "a" }, toBytes(d, o)),
      appendFile: (p, d, o) => call("fs.writeFile", { path: String(p), append: true }, toBytes(d, o)),
      stat: (p) => call("fs.stat", { path: String(p) }),
      readdir: (p) => call("fs.readdir", { path: String(p) }),
      mkdir: (p, o) => call("fs.mkdir", { path: String(p), recursive: !!opts(o).recursive }),
      rm: (p, o) => call("fs.rm", { path: String(p), recursive: !!opts(o).recursive, force: !!opts(o).force }),
      unlink: (p) => call("fs.unlink", { path: String(p) }),
      rmdir: (p, o) => call("fs.rm", { path: String(p), recursive: !!opts(o).recursive, dirOnly: true }),
      rename: (a, b) => call("fs.rename", { path: String(a), dest: String(b) }),
      copyFile: (a, b) => call("fs.copyFile", { path: String(a), dest: String(b) }),
      access: (p) => call("fs.access", { path: String(p) }),
      realpath: (p) => call("fs.realpath", { path: String(p) }),
      exists: (p) => call("fs.exists", { path: String(p) }),
    };
  }
  const sfs = makeFs(op);
  const fsp = makeFs(opAsync);
  const dirents = (list, p, o) => (opts(o).withFileTypes ? list.map((e) => new Dirent(e, p)) : list.map((e) => e.name));
  const fsSync = {
    readFileSync: (p, o) => asResult(sfs.readFile(p, o), o),
    writeFileSync: (p, d, o) => { sfs.writeFile(p, d, o); },
    appendFileSync: (p, d, o) => { sfs.appendFile(p, d, o); },
    existsSync: (p) => { try { return sfs.exists(p); } catch { return false; } },
    statSync: (p, o) => { try { return new Stats(sfs.stat(p)); } catch (e) { if (o && o.throwIfNoEntry === false && e.code === "ENOENT") return undefined; throw e; } },
    readdirSync: (p, o) => dirents(sfs.readdir(p), String(p), o),
    mkdirSync: (p, o) => { sfs.mkdir(p, o); return opts(o).recursive ? path.resolve(String(p)) : undefined; },
    rmSync: (p, o) => { sfs.rm(p, o); },
    rmdirSync: (p, o) => { sfs.rmdir(p, o); },
    unlinkSync: (p) => { sfs.unlink(p); },
    renameSync: (a, b) => { sfs.rename(a, b); },
    copyFileSync: (a, b) => { sfs.copyFile(a, b); },
    accessSync: (p) => { sfs.access(p); },
    realpathSync: (p) => sfs.realpath(p),
    mkdtempSync: (prefix) => {
      const p = prefix + Math.random().toString(36).slice(2, 8);
      sfs.mkdir(p, {});
      return p;
    },
  };
  fsSync.lstatSync = fsSync.statSync;
  const promises = {
    readFile: async (p, o) => asResult(await fsp.readFile(p, o), o),
    writeFile: async (p, d, o) => { await fsp.writeFile(p, d, o); },
    appendFile: async (p, d, o) => { await fsp.appendFile(p, d, o); },
    stat: async (p) => new Stats(await fsp.stat(p)),
    readdir: async (p, o) => dirents(await fsp.readdir(p), String(p), o),
    mkdir: async (p, o) => { await fsp.mkdir(p, o); return opts(o).recursive ? path.resolve(String(p)) : undefined; },
    rm: async (p, o) => { await fsp.rm(p, o); },
    rmdir: async (p, o) => { await fsp.rmdir(p, o); },
    unlink: async (p) => { await fsp.unlink(p); },
    rename: async (a, b) => { await fsp.rename(a, b); },
    copyFile: async (a, b) => { await fsp.copyFile(a, b); },
    access: async (p) => { await fsp.access(p); },
    realpath: (p) => fsp.realpath(p),
    mkdtemp: async (prefix) => {
      const p = prefix + Math.random().toString(36).slice(2, 8);
      await fsp.mkdir(p, {});
      return p;
    },
  };
  promises.lstat = promises.stat;
  // Callback API: fs.readFile(path, [options], callback).
  const callbackApi = {};
  for (const [name, fn] of Object.entries(promises)) {
    callbackApi[name] = (...args) => {
      const cb = typeof args[args.length - 1] === "function" ? args.pop() : null;
      const p = fn(...args);
      if (cb) p.then((v) => cb(null, v), (e) => cb(e));
      return undefined;
    };
  }
  callbackApi.exists = (p, cb) => { promises.access(p).then(() => cb(true), () => cb(false)); };
  const constants = { F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1 };
  const fs = { ...callbackApi, ...fsSync, promises, constants, Stats, Dirent };

  // ---------------------------------------------------------------- events
  class EventEmitter {
    constructor() { this._events = new Map(); }
    on(name, fn) { (this._events.get(name) ?? this._events.set(name, []).get(name)).push(fn); return this; }
    addListener(name, fn) { return this.on(name, fn); }
    once(name, fn) { const w = (...a) => { this.off(name, w); fn.apply(this, a); }; w.listener = fn; return this.on(name, w); }
    off(name, fn) { const l = this._events.get(name); if (l) this._events.set(name, l.filter((x) => x !== fn && x.listener !== fn)); return this; }
    removeListener(name, fn) { return this.off(name, fn); }
    removeAllListeners(name) { name === undefined ? this._events.clear() : this._events.delete(name); return this; }
    emit(name, ...args) {
      const l = this._events.get(name);
      if (!l || l.length === 0) { if (name === "error") throw args[0]; return false; }
      for (const fn of [...l]) fn.apply(this, args);
      return true;
    }
    listenerCount(name) { return (this._events.get(name) ?? []).length; }
    listeners(name) { return [...(this._events.get(name) ?? [])]; }
  }
  EventEmitter.EventEmitter = EventEmitter;

  // ------------------------------------------------------------ util / os
  const promisifyCustom = Symbol.for("nodejs.util.promisify.custom");
  const promisify = (fn) => fn[promisifyCustom] ?? ((...args) => new Promise((res, rej) => fn(...args, (e, v) => (e ? rej(e) : res(v)))));
  promisify.custom = promisifyCustom;
  const util = {
    format,
    inspect,
    promisify,
    isDeepStrictEqual: (a, b) => jsonStringify(a) === jsonStringify(b),
    types: { isPromise: (p) => p instanceof Promise, isDate: (d) => d instanceof Date, isRegExp: (r) => r instanceof RegExp },
    TextEncoder,
    TextDecoder,
  };
  const os = {
    EOL: "\n",
    platform: () => "linux",
    type: () => "Linux",
    arch: () => "x64",
    release: () => "6.0.0-ply",
    hostname: () => env.HOSTNAME ?? "ply",
    homedir: () => env.HOME ?? "/",
    tmpdir: () => "/tmp",
    cpus: () => [],
    totalmem: () => 0,
    freemem: () => 0,
    userInfo: () => ({ username: env.USER ?? "ply", uid: 1000, gid: 1000, homedir: env.HOME ?? "/", shell: null }),
  };

  // --------------------------------------------------------- child_process
  // Every process goes through the IO layer's exec: it runs in a directory
  // on a disk mount and its process group ends with the call.
  const SIGNALS = { 1: "SIGHUP", 2: "SIGINT", 3: "SIGQUIT", 6: "SIGABRT", 9: "SIGKILL", 13: "SIGPIPE", 14: "SIGALRM", 15: "SIGTERM" };
  function processCall(call, argv, o) {
    o = o ?? {};
    const args = { argv: argv.map(String) };
    if (o.cwd !== undefined) args.cwd = String(o.cwd);
    if (o.env) {
      const env = {};
      for (const [k, v] of objectEntries(o.env)) if (v !== undefined) env[k] = String(v);
      args.env = env;
    }
    if (o.timeout) args.timeout = Number(o.timeout);
    if (o.maxBuffer) args.maxOutput = Number(o.maxBuffer);
    const input = o.input === undefined || o.input === null ? undefined : toBytes(o.input);
    return call("proc.exec", args, input);
  }
  function processResult(r) {
    return {
      code: r.code,
      signal: r.signal === null || r.signal === undefined ? null : SIGNALS[r.signal] ?? `SIG${r.signal}`,
      stdout: toBuffer(base64Decode(r.stdout)),
      stderr: toBuffer(base64Decode(r.stderr)),
      timedOut: r.timedOut,
      durationMs: r.durationMs,
      truncated: r.truncated,
    };
  }
  const asOutput = (buf, enc) => (enc && enc !== "buffer" ? decode(buf, enc) : buf);
  function commandError(cmd, r, enc) {
    const err = new Error(`Command failed: ${cmd}${r.stderr.length ? "\n" + decode(r.stderr, "utf8") : ""}`);
    Object.assign(err, { code: r.code, status: r.code, signal: r.signal, killed: r.timedOut, cmd });
    err.stdout = asOutput(r.stdout, enc);
    err.stderr = asOutput(r.stderr, enc);
    return err;
  }
  function fileArgs(file, args, options, callback) {
    if (typeof args === "function") return [String(file), [], {}, args];
    if (!Array.isArray(args)) return fileArgs(file, [], args, options);
    if (typeof options === "function") return [String(file), args, {}, options];
    return [String(file), args, options ?? {}, callback];
  }
  function startAsync(cmd, argv, options, callback) {
    const child = new EventEmitter();
    const enc = options.encoding ?? "utf8";
    processCall(opAsync, argv, options).then(
      (raw) => {
        const r = processResult(raw);
        child.exitCode = r.code;
        child.signalCode = r.signal;
        const err = r.code === 0 ? null : commandError(cmd, r, enc);
        if (callback) callback(err, asOutput(r.stdout, enc), asOutput(r.stderr, enc));
        child.emit("exit", r.code, r.signal);
        child.emit("close", r.code, r.signal);
      },
      (e) => {
        if (callback) callback(e, "", "");
        else child.emit("error", e);
      },
    );
    return child;
  }
  function runSync(cmd, argv, options) {
    const r = processResult(processCall(op, argv, options));
    if (r.code !== 0) throw commandError(cmd, r, options.encoding);
    return asOutput(r.stdout, options.encoding);
  }
  const shellOf = (o) => (typeof o.shell === "string" ? o.shell : "/bin/sh");
  const childProcess = {
    execFile(file, args, options, callback) {
      [file, args, options, callback] = fileArgs(file, args, options, callback);
      return startAsync([file, ...args].join(" "), [file, ...args], options, callback);
    },
    exec(command, options, callback) {
      if (typeof options === "function") [options, callback] = [{}, options];
      options = options ?? {};
      return startAsync(String(command), [shellOf(options), "-c", String(command)], options, callback);
    },
    execFileSync(file, args, options) {
      [file, args, options] = fileArgs(file, args, options);
      return runSync([file, ...args].join(" "), [file, ...args], options);
    },
    execSync(command, options) {
      options = options ?? {};
      return runSync(String(command), [shellOf(options), "-c", String(command)], options);
    },
    spawnSync(file, args, options) {
      [file, args, options] = fileArgs(file, args, options);
      const r = processResult(processCall(op, [file, ...args], options));
      const stdout = asOutput(r.stdout, options.encoding);
      const stderr = asOutput(r.stderr, options.encoding);
      return { pid: 0, output: [null, stdout, stderr], stdout, stderr, status: r.code, signal: r.signal };
    },
    spawn() {
      const err = new Error("child_process.spawn is not supported yet (no streaming processes); use execFile or exec");
      err.code = "ERR_PLY_UNSUPPORTED";
      throw err;
    },
  };
  for (const name of ["exec", "execFile"]) {
    childProcess[name][promisifyCustom] = (...args) =>
      new Promise((resolve, reject) =>
        childProcess[name](...args, (err, stdout, stderr) => (err ? reject(err) : resolve({ stdout, stderr }))),
      );
  }

  // --------------------------------------------------------------- plugins
  // Imported from ply:plugins; kept current by the kernel.
  //
  // - `await plugins.bash(input)` runs its `onHandle` lazily: nothing runs
  //   until it is awaited, `then` is the call, and the kernel schedules it
  //   under the caller. Awaiting a handle twice runs it once, like a promise.
  // - What its `onLoad` returned; the kernel loads named imports first.
  // - `description`, and for plugins with `onHandle` `parameters` (JSON
  //   Schema of its tool arguments) and `wrapsInput`.
  function callHandle(plugin, input) {
    let started = null;
    const start = () => (started ??= opAsync("kernel.spawn", { plugin, input: input === undefined ? null : input }));
    return objectFreeze({
      then: (onDone, onFailed) => start().then(onDone, onFailed),
      catch: (onFailed) => start().catch(onFailed),
      finally: (onSettled) => start().finally(onSettled),
    });
  }
  const directory = () => op("kernel.directory") ?? [];
  const findPlugin = (name) => directory().find((p) => p.name === name);
  const pluginEntries = new Map();
  function pluginEntry(name) {
    if (pluginEntries.has(name)) return pluginEntries.get(name);
    const entry = (input) => callHandle(name, input);
    const methods = new Map();
    const metadata = ["description", "parameters", "wrapsInput"];
    const read = (key) => {
      if (key === "then") return undefined;
      if (methods.has(key)) return methods.get(key);
      const known = findPlugin(name);
      if (key === "description") return known?.description;
      if (key === "parameters") return known?.tool?.parameters;
      if (key === "wrapsInput") return known?.tool?.wraps_input;
      if (typeof key === "string" && known) {
        if (objectHasOwn(known.mounts.data, key)) return known.mounts.data[key];
        if (known.mounts.methods.includes(key)) {
          if (!methods.has(key)) methods.set(key, (...args) => opAsync("kernel.method", { plugin: name, method: key, args }));
          return methods.get(key);
        }
      }
      return Reflect.get(entry, key);
    };
    const handle = new Proxy(entry, {
      get: (_, key) => read(key),
      has: (_, key) => metadata.includes(key) || key in entry
        || !!findPlugin(name)?.mounts.methods.includes(key) || objectHasOwn(findPlugin(name)?.mounts.data ?? {}, key),
      ownKeys: () => [...new Set([...Reflect.ownKeys(entry), ...metadata, ...Object.keys(findPlugin(name)?.mounts.data ?? {}), ...(findPlugin(name)?.mounts.methods ?? [])])],
      getOwnPropertyDescriptor: (_, key) => Reflect.getOwnPropertyDescriptor(entry, key) ??
        (typeof key === "string" && (metadata.includes(key) || objectHasOwn(findPlugin(name)?.mounts.data ?? {}, key) || findPlugin(name)?.mounts.methods.includes(key))
          ? { get: () => read(key), enumerable: true, configurable: true } : undefined),
      set: () => false, defineProperty: () => false, deleteProperty: () => false,
    });
    pluginEntries.set(name, handle);
    return handle;
  }
  const plugins = new Proxy(objectCreate(null), {
    // `then` is excluded so `await plugins` is not mistaken for a call.
    get: (_, name) => (typeof name === "string" && name !== "then" ? pluginEntry(name) : undefined),
    has: (_, name) => typeof name === "string" && findPlugin(name) !== undefined,
    ownKeys: () => directory().map((p) => p.name),
    getOwnPropertyDescriptor: (_, name) =>
      typeof name === "string" && findPlugin(name) !== undefined
        ? { value: pluginEntry(name), enumerable: true, configurable: true, writable: false }
        : undefined,
    set: () => false,
    defineProperty: () => false,
    deleteProperty: () => false,
  });

  // --------------------------------------------------------------- globals
  Object.assign(G, {
    console, process, Buffer, TextEncoder, TextDecoder,
    setTimeout, setInterval, setImmediate, clearTimeout, clearInterval: clearTimeout, clearImmediate: clearTimeout,
    queueMicrotask,
  });
  if (!G.structuredClone) G.structuredClone = (v) => jsonParse(jsonStringify(v));

  // ---------------------------------------------------------------- modules
  const BUILTIN = {
    fs, "fs/promises": promises, path, "path/posix": path, process, buffer: { Buffer }, events: EventEmitter, util, os,
    timers: { setTimeout, setInterval, setImmediate, clearTimeout, clearInterval: clearTimeout },
    "timers/promises": { setTimeout: (ms, v) => new Promise((r) => setTimeout(() => r(v), ms)) },
    child_process: childProcess,
  };
  const namespaces = new Map();
  const eventCallbacks = new Map();
  const eventSubscriptions = new Map();
  const commandCallbacks = new Map();
  let nextEventCallback = 0;
  let nextCommandCallback = 0;
  function builtinNamespace(name) {
    if (name.startsWith("ply:async/helpers/")) {
      const helper = asyncHelpers[name.slice("ply:async/helpers/".length)];
      if (typeof helper !== "function") throw new TypeError("unknown async helper");
      return objectFreeze({ default: helper });
    }
    if (!namespaces.has(name)) {
      if (name === "ply:plugins") {
        // Both imports resolve to the same stable, lazily populated handles.
        // The namespace stays live while later plugin onLoad hooks complete.
        const ns = new Proxy(objectCreate(null), {
          get: (_, key) => key === "default" ? plugins : key === "__esModule" ? true :
            key === Symbol.toStringTag ? "Module" : typeof key === "string" && key !== "then" ? pluginEntry(key) : undefined,
          has: (_, key) => key === "default" || (typeof key === "string" && key !== "then" && key !== "__esModule"),
          ownKeys: () => ["default", ...directory().map(p => p.name).filter(n => n !== "default" && n !== "then" && n !== "__esModule")],
          getOwnPropertyDescriptor: (_, key) => typeof key === "string" && key !== "then" ?
            { get: () => key === "default" ? plugins : key === "__esModule" ? true : pluginEntry(key), enumerable: true, configurable: true } : undefined,
          set: () => false, defineProperty: () => false, deleteProperty: () => false,
        });
        namespaces.set(name, ns);
        return ns;
      }
      // The browser bundle and its fetch/stream scope are instantiated once
      // in this realm, only when a plugin imports the embedded model layer.
      const m = name === "ply:models" ? compile(op("models.source"))({
        credentials: (profile) => op("models.credentials", profile),
        open: (args, body) => op("http.open", args, body && plain(body)),
        headers: (id) => opAsync("http.headers", { id }),
        read: (id) => opAsync("http.read", { id }),
        abort: (id) => op("http.abort", { id }),
      }) : name === "ply:http" ? {
        hasCredentials: provider => op("http.auth_configured", { provider }),
        requestJson: request => opAsync("http.request_json", request),
      } : name === "ply:skills" ? {
        parse: source => op("skills.parse", { source }),
      } : name === "ply:markdown" ? compile(op("markdown.source"))({ parse: source => op("markdown.parse", { source }) }) : name === "ply:terminal" ? {
        open: () => op("terminal.open", {}),
        render: (id, lines, options = {}) => op("terminal.render", { id, lines, options }),
        flush: id => op("terminal.flush", { id }),
        read: (id, timeout_ms = 100) => opAsync("terminal.read", { id, timeout_ms }),
        close: id => op("terminal.close", { id }),
      } : name === "ply:sh" ? {
        exec: request => opAsync("sh.exec", { ...request, env: process.env }),
      } : name === "ply:commands" ? {
        async register(command) {
          if (!command || typeof command.handler !== "function") throw new TypeError("command handler must be a function");
          const id = ++nextCommandCallback;
          commandCallbacks.set(id, command.handler);
          try {
            await opAsync("kernel.command_register", { name: command.name, description: command.description, callback: id });
          } catch (error) { commandCallbacks.delete(id); throw error; }
        },
        list: () => opAsync("kernel.command_list", {}),
        run: (name, input) => opAsync("kernel.command_run", { name, input: input === undefined ? null : input }),
      } : name === "ply:events" ? {
        define: name => opAsync("kernel.event_define", { name }),
        async on(name, callback) {
          if (typeof callback !== "function") throw new TypeError("event listener must be a function");
          const id = ++nextEventCallback;
          eventCallbacks.set(id, callback);
          try {
            const subscription = await opAsync("kernel.event_on", { name, callback: id });
            eventSubscriptions.set(subscription, id);
            return subscription;
          } catch (error) { eventCallbacks.delete(id); throw error; }
        },
        async off(subscription) {
          const removed = await opAsync("kernel.event_off", { subscription });
          const callback = eventSubscriptions.get(subscription);
          if (callback !== undefined) eventCallbacks.delete(callback);
          eventSubscriptions.delete(subscription);
          return removed;
        },
        emit: (name, data, options = {}) => opAsync("kernel.event_emit", { name, data, timeout_ms: options.timeout_ms ?? 30000 }),
        list: () => opAsync("kernel.event_list", {}),
      } : name === "ply:memory-store" ? {
        configure: options => opAsync("memory.configure", options ?? {}),
        recall: options => opAsync("memory.recall", options ?? {}),
        remember: options => opAsync("memory.remember", options),
        forget: options => opAsync("memory.forget", options),
      } : name === "ply:checkpoints" ? {
        create: (snapshot) => opAsync("checkpoint.create", { snapshot }),
        claim: (id) => opAsync("checkpoint.claim", { id }),
        update: (lease, snapshot) => opAsync("checkpoint.update", { lease, snapshot }),
        finish: (lease, remove) => opAsync("checkpoint.finish", { lease, remove }),
      } : BUILTIN[name];
      const ns = Object.assign(Object.create(null), m);
      ns.default = m;
      namespaces.set(name, ns);
    }
    return namespaces.get(name);
  }
  const modules = new Map();
  function load(spec, from) {
    const r = op("module.resolve", { spec, from });
    if (r.builtin) return { exports: builtinNamespace(r.builtin), done: Promise.resolve() };
    let rec = modules.get(r.id);
    if (rec) return rec;
    const code = op("module.load", { id: r.id });
    const fn = compile(code + "\n//# sourceURL=" + r.id);
    const exports = Object.create(null);
    rec = { exports, done: Promise.resolve() };
    modules.set(r.id, rec);
    const require = (s) => load(s, r.id).exports;
    const dynamicImport = (s) => {
      try {
        const m = load(String(s), r.id);
        return m.done.then(() => m.exports);
      } catch (e) {
        return Promise.reject(e);
      }
    };
    const meta = { url: "file://" + r.id, filename: r.id, dirname: r.dir, main: from === null };
    try {
      const result = fn(exports, require, dynamicImport, meta);
      if (result && typeof result.then === "function") rec.done = result;
    } catch (e) {
      modules.delete(r.id);
      throw e;
    }
    return rec;
  }

  // ------------------------------------------------------ host entry points
  function describe(e) {
    if (e instanceof Error) return `${e.name}: ${e.message}`;
    return fmt(e, 0, [], true, 2);
  }
  function settle(id, run) {
    let p;
    try {
      p = Promise.resolve(run());
    } catch (e) {
      p = Promise.reject(e);
    }
    p.then(
      (v) => {
        let json;
        try {
          json = jsonStringify({ id, ok: true, value: v === undefined ? null : v });
        } catch (e) {
          json = jsonStringify({ id, ok: false, error: "result is not JSON-serializable: " + e.message });
        }
        host("settle", json);
      },
      (e) => host("settle", jsonStringify({
        id,
        ok: false,
        error: describe(e),
        stack: e instanceof Error && typeof e.stack === "string" ? e.stack : null,
        exit: e && e[EXIT] !== undefined ? e[EXIT] : null,
      })),
    );
  }
  // A plugin's entry module, loaded once per realm, and what its onLoad
  // returned.
  const entries = new Map();
  const loadedMounts = new Map();
  async function entry(file) {
    if (!entries.has(file)) entries.set(file, load(file, null));
    const rec = entries.get(file);
    await rec.done;
    return rec.exports;
  }
  asyncHelpers.setUnhandled(reason => op("call.unhandled", { error: describe(reason), stack: reason?.stack ?? null }));
  return {
    main: (id, file) => settle(id, async () => {
      const rec = load(file, null);
      entries.set(file, rec);
      await rec.done;
      return Object.keys(rec.exports);
    }),
    call: (id, file, exportPath, argsJson) => settle(id, async () => {
      const rec = load(file, null);
      await rec.done;
      let target = rec.exports;
      let owner;
      for (const part of exportPath.split(".").filter(Boolean)) {
        owner = target;
        target = target == null ? undefined : target[part];
      }
      if (typeof target !== "function") throw new TypeError(`${exportPath} is not a function`);
      return await target.apply(owner, jsonParse(argsJson));
    }),
    eval: (id, code) => settle(id, () => evaluate(indirectEval, asyncHelpers, code)),
    // One call; the host has tagged this entry with its scope.
    run: (id, file, inputJson) => settle(id, async () => {
      const exports = await entry(file);
      if (typeof exports.onHandle !== "function") throw new TypeError(`${file} does not export an onHandle function`);
      return await exports.onHandle(jsonParse(inputJson));
    }),
    // onLoad, once: keep what it returns; tell the host what is in it.
    load: (id, file, optionsJson) => settle(id, async () => {
      const exports = await entry(file);
      const mounted = typeof exports.onLoad === "function" ? await exports.onLoad(jsonParse(optionsJson)) : undefined;
      if (mounted !== undefined && mounted !== null && (typeof mounted !== "object" || Array.isArray(mounted))) {
        throw new TypeError(`${file}: onLoad must return an object (or nothing)`);
      }
      const data = {};
      const methods = [];
      for (const [key, value] of objectEntries(mounted ?? {})) {
        if (typeof value === "function") methods.push(key);
        else data[key] = value;
      }
      loadedMounts.set(file, mounted ?? {});
      return { data, methods };
    }),
    // Callback stays in its subscribing realm; the kernel schedules the call.
    event: (id, callback, inputJson) => settle(id, async () => {
      const handler = eventCallbacks.get(Number(callback));
      if (typeof handler !== "function") throw new Error("event subscription was removed");
      return await handler(jsonParse(inputJson));
    }),
    command: (id, callback, inputJson) => settle(id, async () => {
      const handler = commandCallbacks.get(Number(callback));
      if (typeof handler !== "function") throw new Error("command registration was removed");
      return await handler(jsonParse(inputJson));
    }),
    // A function onLoad returned, called from another realm.
    method: (id, file, name, argsJson) => settle(id, async () => {
      const mounted = loadedMounts.get(file);
      if (!mounted || !objectHasOwn(mounted, name) || typeof mounted[name] !== "function") {
        throw new TypeError(`no function ${name} was mounted by ${file}`);
      }
      return await mounted[name](...jsonParse(argsJson));
    }),
    onUnhandledRejection(promise, reason) {
      // A cancelled call rejects everything it was awaiting; that is not news.
      if (reason?.code !== "ERR_PLY_CANCELLED") {
        const text = describe(reason) + (reason instanceof Error && reason.stack ? "\n" + reason.stack : "");
        write(2, "Uncaught (in promise) " + text + "\n");
      }
      asyncHelpers.onUnhandled(promise, reason);
    },
    // onDispose, once, as the realm is retired; only after its entry evaluated.
    dispose: (id, file) => settle(id, async () => {
      const rec = entries.get(file);
      if (!rec) return;
      try { await rec.done; } catch { return; }
      if (typeof rec.exports.onDispose === "function") await rec.exports.onDispose();
    }),
    // What an interrupted call's pending promises reject with.
    abortError: (message) => objectAssign(new Error(message), { name: "AbortError", code: "ERR_PLY_CANCELLED" }),
    abortRunning: (message) => asyncHelpers.abortRunning(objectAssign(new Error(message), { name: "AbortError", code: "ERR_PLY_CANCELLED" })),
    discardTimers(ids) { for (const id of ids) timers.delete(id); },
    fire,
  };
});
