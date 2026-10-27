import fs from "node:fs";
import path from "node:path";

// Plugins see capabilities, never scheduling (RFC 0012): no fiber, caller
// identity or load waits in plugin-visible code or the runtime prelude.
const root = path.resolve(import.meta.dirname, "..");
const forbidden = [/fiber/i, /ply:lifecycle/, /\bctx\b/, /\.loaded\b/];
const files = [];
const walk = dir => {
  for (const entry of fs.readdirSync(path.join(root, dir), { withFileTypes: true })) {
    const relative = path.join(dir, entry.name);
    if (entry.isDirectory()) { if (entry.name !== "node_modules") walk(relative); }
    else if (/\.(ts|js|mjs)$/.test(entry.name)) files.push(relative);
  }
};
for (const dir of ["plugins", "examples/plugins", "packages/sdk"]) walk(dir);
files.push("crates/ply-jsc/src/prelude.js", "crates/ply-jsc/src/async.js");
const failures = [];
for (const file of files) {
  fs.readFileSync(path.join(root, file), "utf8").split("\n").forEach((line, index) => {
    for (const pattern of forbidden) if (pattern.test(line)) failures.push(`${file}:${index + 1}: ${pattern} ${line.trim()}`);
  });
}
if (failures.length) {
  process.stderr.write(failures.join("\n") + "\n");
  process.exitCode = 1;
} else console.log(`Checked ${files.length} plugin-visible files for scheduling leaks.`);
