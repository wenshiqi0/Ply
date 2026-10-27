// The errors fs reports: code, syscall, path and message.
import fs from "node:fs";
import fsp from "node:fs/promises";
const show = (label, value) => console.log(label, JSON.stringify(value));
const describe = (e) => ({ code: e.code, syscall: e.syscall, path: e.path, message: e.message, error: e instanceof Error });
const attempt = (label, fn) => { try { fn(); show(label, "ok"); } catch (e) { show(label, describe(e)); } };
fs.mkdirSync("dir");
fs.writeFileSync("dir/file", "x");
attempt("read missing", () => fs.readFileSync("missing.txt"));
attempt("mkdir existing", () => fs.mkdirSync("dir"));
attempt("read directory", () => fs.readFileSync("dir"));
attempt("readdir file", () => fs.readdirSync("dir/file"));
attempt("rmdir non-empty", () => fs.rmdirSync("dir"));
attempt("unlink missing", () => fs.unlinkSync("missing"));
attempt("stat missing", () => fs.statSync("missing"));
attempt("file as directory", () => fs.writeFileSync("dir/file/child", "x"));
try { await fsp.readFile("missing.txt"); } catch (e) { show("promise read missing", describe(e)); }
