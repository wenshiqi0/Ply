import path from "node:path";
import type { EditInput, EditResult } from "./types";
import { context } from "ply:plugins";
import { readFile, stat, writeFile } from "node:fs/promises";

/**
 * Replace an exact string in a file. old_string must occur exactly once
 * unless replace_all is set; include enough surrounding text to make it
 * unique.
 */
export async function onHandle(input: EditInput): Promise<EditResult> {
  const { path: file, old_string: before, new_string: after } = input ?? ({} as typeof input);
  if (typeof file !== "string" || file === "") throw new Error("'path' must be a non-empty string");
  if (typeof before !== "string" || before === "") throw new Error("'old_string' must be a non-empty string");
  if (typeof after !== "string") throw new Error("'new_string' must be a string");
  if (before === after) throw new Error("old_string and new_string are the same");
  const isFile = await stat(file).then((s) => s.isFile(), () => false);
  if (!isFile) throw new Error(`${file}: no such file (use create for new files)`);

  const text = await readFile(file, "utf8");
  const found = text.split(before).length - 1;
  if (found === 0) throw new Error(`old_string was not found in ${file}`);
  if (found > 1 && !input.replace_all) throw new Error(`old_string occurs ${found} times in ${file}; add context or set replace_all`);
  await writeFile(file, input.replace_all ? text.split(before).join(after) : text.replace(before, () => after));
  return { path: path.resolve(file), replacements: input.replace_all ? found : 1 };
}

/** Offer this plugin to the model as a tool. */
export async function onLoad() {
  await context.registerTool("edit");
}
