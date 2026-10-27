import fs from "node:fs";
import path from "node:path";

// Plugins see capabilities, never scheduling (RFC 0012): no fiber, caller
// identity or load waits in plugin-visible code or the runtime prelude.
// Libraries come from npm, not `ply:` modules (RFC 0013): the removed
// model module stays gone. CommonJS `module.loaded` is Node's own field.
const root = path.resolve(import.meta.dirname, "..");
const forbidden = [/fiber/i, /ply:lifecycle/, /\bctx\b/, /(?<!\bmodule)\.loaded\b/, /ply:models/];
const files = [];
const walk = dir => {
  for (const entry of fs.readdirSync(path.join(root, dir), { withFileTypes: true })) {
    const relative = path.join(dir, entry.name);
    if (entry.isDirectory()) { if (entry.name !== "node_modules") walk(relative); }
    else if (/\.(ts|js|mjs)$/.test(entry.name)) files.push(relative);
  }
};
for (const dir of ["plugins", "examples/plugins", "packages/sdk"]) walk(dir);
// "ply:runtime" is the prelude's internals for the runtime's own modules
// (RFC 0015); nothing plugin code is given may name it.
const pluginVisible = new Set(files);
walk("crates/ply-jsc/src/node");
files.push("crates/ply-jsc/src/prelude.js", "crates/ply-jsc/src/async.js", "crates/ply-jsc/src/web.js");
const failures = [];
for (const file of files) {
  fs.readFileSync(path.join(root, file), "utf8").split("\n").forEach((line, index) => {
    for (const pattern of forbidden) if (pattern.test(line)) failures.push(`${file}:${index + 1}: ${pattern} ${line.trim()}`);
    if (pluginVisible.has(file) && /ply:runtime/.test(line)) failures.push(`${file}:${index + 1}: /ply:runtime/ ${line.trim()}`);
  });
}
if (failures.length) {
  process.stderr.write(failures.join("\n") + "\n");
  process.exitCode = 1;
} else console.log(`Checked ${files.length} plugin-visible files for scheduling leaks and removed modules.`);
