import plugins from "wendy:plugins";
import { readFile } from "node:fs/promises";
import path from "node:path";

/**
 * Read UTF-8 text from a mounted file. Prefer this tool over bash for reading
 * files. Use offset and limit to read a range of lines (default: first 200).
 */
export async function onHandle(input: {
  /** File to read, relative to the workspace or an absolute mounted path. */
  path: string;
  /** First line to read, starting at 1 (default 1). */
  offset?: number;
  /** Maximum number of lines to return (default 200). */
  limit?: number;
}): Promise<{ path: string; content: string; offset: number; lines: number; total_lines: number; has_more: boolean }> {
  if (typeof input?.path !== "string" || input.path === "") throw new Error("'path' must be a non-empty string");
  const offset = input.offset ?? 1;
  const limit = input.limit ?? 200;
  if (!Number.isSafeInteger(offset) || offset < 1) throw new Error("'offset' must be a positive safe integer");
  if (!Number.isSafeInteger(limit) || limit < 1) throw new Error("'limit' must be a positive safe integer");

  const text = await readFile(input.path, "utf8");
  // Keep line endings, without counting a final newline as an extra empty line.
  const allLines = text.match(/[^\n]*\n|[^\n]+$/g) ?? [];
  if (offset > Math.max(1, allLines.length)) throw new Error(`'offset' ${offset} is beyond the end of the file (${allLines.length} lines)`);
  const selected = allLines.slice(offset - 1, offset - 1 + limit);
  return {
    path: path.resolve(input.path),
    content: selected.join(""),
    offset,
    lines: selected.length,
    total_lines: allLines.length,
    has_more: offset - 1 + selected.length < allLines.length,
  };
}

/** Offer this plugin to the model as a tool. */
export async function onLoad() {
  await plugins.context.loaded;
  await ctx.registerTool("read");
}
