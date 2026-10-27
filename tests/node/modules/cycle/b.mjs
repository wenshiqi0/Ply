import { a } from "./a.mjs";
export let b = "b-initialised";
export function readA() { return a; }
let early;
try { early = a; } catch (e) { early = e.name; }
export { early };
