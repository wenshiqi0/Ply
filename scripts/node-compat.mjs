// Node compatibility cases (RFC 0014): tests/node/<category>/<case>.mjs.
//   lint    the pinned version agrees with @types/node; every case has an
//           expectation and every Ply-only expectation states its reason
//   record  run each case with the pinned Node, write <case>.expected
//   check   run each case with the pinned Node, compare with <case>.expected
// Ply runs the same cases in crates/ply-cli/tests/node_compat.rs.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const suite = path.join(root, "tests", "node");
const pinned = fs.readFileSync(path.join(root, ".node-version"), "utf8").trim();
const fail = (message) => { console.error(message); process.exit(1); };

function cases() {
  return fs.readdirSync(suite, { withFileTypes: true }).filter((d) => d.isDirectory()).flatMap((category) =>
    fs.readdirSync(path.join(suite, category.name)).filter((name) => name.endsWith(".mjs")).sort()
      .map((name) => ({ category: category.name, name, file: path.join(suite, category.name, name) })));
}

/** A case's stdout, run in a fresh copy of its category, with that copy's path as `<dir>`. */
export function run(command, args, { category, name }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ply-node-compat-"));
  try {
    fs.cpSync(path.join(suite, category), dir, { recursive: true });
    let stdout, status = 0;
    try {
      stdout = execFileSync(command, [...args, name], { cwd: dir, env: { PATH: process.env.PATH }, encoding: "utf8", timeout: 30000, stdio: ["ignore", "pipe", "ignore"] });
    } catch (error) {
      if (typeof error.status !== "number") throw error;
      ({ stdout, status } = error);
    }
    const real = fs.realpathSync(dir);
    let out = stdout.split(real).join("<dir>").split(dir).join("<dir>");
    if (status !== 0) out += `[exit ${status}]\n`;
    return out;
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

function lint() {
  if (!/^\d+\.\d+\.\d+$/.test(pinned)) fail(`.node-version must be an exact version, not "${pinned}"`);
  const types = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8")).devDependencies["@types/node"];
  const [major, minor] = pinned.split(".").map(Number);
  const [typesMajor, typesMinor] = types.split(".").map(Number);
  // Types may not declare APIs newer than the runtime they describe.
  if (typesMajor !== major || typesMinor > minor) fail(`@types/node ${types} does not describe Node ${pinned}`);
  const problems = [];
  for (const c of cases()) {
    if (!fs.existsSync(c.file.replace(/\.mjs$/, ".expected"))) problems.push(`${c.category}/${c.name}: no .expected (run: npm run compat:record)`);
    const ply = c.file.replace(/\.mjs$/, ".ply.expected");
    if (fs.existsSync(ply) && !/^\/\/ ply-divergence: \S/m.test(fs.readFileSync(c.file, "utf8"))) {
      problems.push(`${c.category}/${c.name}: .ply.expected without a "// ply-divergence: <reason>" line`);
    }
  }
  if (problems.length) fail(problems.join("\n"));
  console.log(`Node ${pinned}: ${cases().length} compatibility cases, types @types/node ${types}.`);
}

function withPinnedNode(each) {
  if (process.version !== `v${pinned}`) fail(`record and check need Node ${pinned} (.node-version), not ${process.version}`);
  let failures = 0;
  for (const c of cases()) failures += each(c, run(process.execPath, [], c)) ? 0 : 1;
  return failures;
}

const mode = process.argv[2];
if (mode === "lint") lint();
else if (mode === "record") {
  withPinnedNode((c, out) => { fs.writeFileSync(c.file.replace(/\.mjs$/, ".expected"), out); return true; });
  console.log(`Recorded ${cases().length} cases with Node ${pinned}.`);
} else if (mode === "check") {
  const failures = withPinnedNode((c, out) => {
    const expected = fs.readFileSync(c.file.replace(/\.mjs$/, ".expected"), "utf8");
    if (out === expected) return true;
    console.error(`${c.category}/${c.name}: Node ${pinned} output differs from .expected\n--- expected\n${expected}--- actual\n${out}`);
    return false;
  });
  if (failures) fail(`${failures} case(s) no longer match Node ${pinned}.`);
  console.log(`${cases().length} cases match Node ${pinned}.`);
} else fail("usage: node scripts/node-compat.mjs lint|record|check");
