import { readFile } from "node:fs/promises";
import path from "node:path";

/**
 * Read a text file. Lines come back numbered like `cat -n`; use offset and
 * limit for long files.
 */
export async function handle(input: {
  /** File to read (relative to the workspace, or absolute). */
  path: string;
  /** First line to return, counting from 1. */
  offset?: number;
  /** How many lines to return (default 2000). */
  limit?: number;
}): Promise<{ path: string; content: string; total_lines: number; truncated: boolean }> {
  if (typeof input?.path !== "string" || input.path === "") throw new Error("'path' must be a non-empty string");
  const offset = input.offset ?? 1;
  const limit = input.limit ?? 2000;
  if (!Number.isInteger(offset) || offset < 1) throw new Error("'offset' must be an integer from 1");
  if (!Number.isInteger(limit) || limit < 1) throw new Error("'limit' must be a positive integer");

  const lines = (await readFile(input.path, "utf8")).split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  const slice = lines.slice(offset - 1, offset - 1 + limit);
  return {
    path: path.resolve(input.path),
    content: slice.map((line, i) => `${String(offset + i).padStart(6)}\t${line}`).join("\n"),
    total_lines: lines.length,
    truncated: offset - 1 + slice.length < lines.length,
  };
}
