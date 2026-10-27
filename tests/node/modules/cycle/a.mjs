import { b } from "./b.mjs";
export let a = "a0";
export const seenFromA = b;
export function setA(value) { a = value; }
