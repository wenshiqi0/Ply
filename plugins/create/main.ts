import plugins from "wendy:plugins";
import { mkdir, stat, writeFile } from "node:fs/promises";
import path from "node:path";

/**
 * Create a file with the given content, and its parent directories. Refuses
 * to replace an existing file unless overwrite is set.
 */
export async function onHandle(input: {
  path: string;
  content: string;
  /** Replace the file if it exists. */
  overwrite?: boolean;
}): Promise<{ path: string; bytes: number; overwritten: boolean }> {
  if (typeof input?.path !== "string" || input.path === "") throw new Error("'path' must be a non-empty string");
  if (typeof input.content !== "string") throw new Error("'content' must be a string");
  const existing = await stat(input.path).then((s) => (s.isDirectory() ? "dir" : "file"), () => null);
  if (existing === "dir") throw new Error(`${input.path} is a directory`);
  if (existing === "file" && !input.overwrite) throw new Error(`${input.path} already exists (set overwrite to replace it, or use edit)`);

  const parent = path.dirname(path.resolve(input.path));
  if (!(await stat(parent).then(() => true, () => false))) await mkdir(parent, { recursive: true });
  await writeFile(input.path, input.content);
  return { path: path.resolve(input.path), bytes: Buffer.byteLength(input.content), overwritten: existing === "file" };
}

/** Offer this plugin to the model as a tool. */
export async function onLoad() {
  await plugins.context.loaded;
  await ctx.registerTool("create");
}
