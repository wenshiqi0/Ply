import { rm, stat } from "node:fs/promises";
import path from "node:path";

/** Remove a file, or a directory and everything in it when recursive is set. */
export async function onHandle(input: {
  path: string;
  /** Required to remove a directory. */
  recursive?: boolean;
}): Promise<{ path: string; kind: "file" | "dir" }> {
  if (typeof input?.path !== "string" || input.path === "") throw new Error("'path' must be a non-empty string");
  const kind = await stat(input.path).then((s) => (s.isDirectory() ? "dir" : "file"), () => null);
  if (kind === null) throw new Error(`${input.path}: no such file or directory`);
  if (kind === "dir" && !input.recursive) throw new Error(`${input.path} is a directory (set recursive to remove it)`);
  await rm(input.path, { recursive: Boolean(input.recursive) });
  return { path: path.resolve(input.path), kind };
}

/** Offer this plugin to the model as a tool. */
export async function onLoad() {
  await tools.context.loaded;
  await ctx.registerTool("remove");
}
