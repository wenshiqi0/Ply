// Package lookup: exports conditions and patterns, main without an
// extension, the nearest node_modules first, and the errors Node reports.
import esm, { kind } from "esm-pkg";
import { a } from "esm-pkg/sub/a";
import cjs from "@scope/cjs-pkg";
import mainPkg from "main-pkg";
import shadow from "shadow";
import inner from "./sub/entry.mjs";
import sync from "sync-pkg";
const show = (label, value) => console.log(label, JSON.stringify(value));
show("exports", { esm, kind, a, sync });
show("main", { cjs: Object.keys(cjs).sort(), mainPkg });
show("nearest", { root: shadow, sub: inner });
for (const spec of ["esm-pkg/internal/x", "esm-pkg/missing", "missing-pkg", "@scope/missing", "node:nope", "./missing.mjs"]) {
  try { await import(spec); show(spec, "loaded"); } catch (e) { show(spec, e.code); }
}
