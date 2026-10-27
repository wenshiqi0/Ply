// CommonJS and ES modules in both directions.
import cjs, { add, helper } from "@scope/cjs-pkg";
import plain from "./plain.js";
import modern from "./modern.js";
import "./script.js";
import req from "./req.cjs";
const show = (label, value) => console.log(label, JSON.stringify(value));
show("esm imports cjs", { add: add(2, 3), helper, esm: cjs.esm, json: cjs.json, dirname: cjs.dirname, filename: cjs.filename, self: cjs.self, asyncValue: await cjs.asyncValue() });
show("detected", { plain, modern, script: globalThis.scriptValue });
const required = req.esm();
show("cjs requires esm", { keys: Object.keys(required), a: required.a, esModule: required.__esModule ?? null });
show("cjs requires esm with default", req.sync());
show("cjs requires top-level await", req.tla());
show("cjs requires json", req.json());
