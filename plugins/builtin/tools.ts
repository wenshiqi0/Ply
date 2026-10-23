// wendy's built-in tools. Every call is one IPO operation run by the kernel
// (input → process → output), traced with everything it did.
//
// read / edit / create / delete go through wendy's own IO layer: only
// mounted storage exists, every call passes the gate and lands in the trace.
// bash runs the system bash: writes confined to read-write disk mounts,
// network closed unless wendy allows it, and the files it changed observed
// and returned.
import { definePlugin } from "wendy";
import fs from "node:fs/promises";
import path from "node:path";

const DEFAULT_TIMEOUT_MS = 120_000;
const MAX_TIMEOUT_MS = 600_000;
const DEFAULT_READ_LINES = 2000;

type Input = Record<string, unknown>;

function fail(message: string): never {
  throw new Error(message);
}

function str(input: Input, key: string, { optional = false, allowEmpty = false } = {}): string | undefined {
  const value = input[key];
  if (value === undefined || value === null) {
    if (optional) return undefined;
    fail(`'${key}' is required`);
  }
  if (typeof value !== "string") fail(`'${key}' must be a string`);
  if (!allowEmpty && value === "") fail(`'${key}' must not be empty`);
  return value;
}

function int(input: Input, key: string, min: number, max: number): number | undefined {
  const value = input[key];
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "number" || !Number.isInteger(value) || value < min || value > max) {
    fail(`'${key}' must be an integer between ${min} and ${max}`);
  }
  return value;
}

function bool(input: Input, key: string): boolean {
  const value = input[key];
  if (value === undefined || value === null) return false;
  if (typeof value !== "boolean") fail(`'${key}' must be a boolean`);
  return value;
}

function object(raw: unknown): Input {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) fail("input must be an object");
  return raw as Input;
}

async function kindOf(p: string): Promise<"file" | "dir" | null> {
  try {
    return (await fs.stat(p)).isDirectory() ? "dir" : "file";
  } catch (e: any) {
    if (e.code === "ENOENT") return null;
    throw e;
  }
}

function count(haystack: string, needle: string): number {
  let n = 0;
  for (let at = haystack.indexOf(needle); at !== -1; at = haystack.indexOf(needle, at + needle.length)) n++;
  return n;
}

export default definePlugin({
  name: "builtin",
  description: "wendy's built-in tools: bash, read, edit, create, delete",
  version: "0.1.0",
  tools: {
    bash: {
      description:
        "Run a command with the system bash. It may write only inside the read-write workspace mounts, " +
        "the network is closed unless wendy was started with it open, and processes it leaves behind are " +
        "stopped when it exits. Returns the exit code, stdout, stderr and the files the command changed.",
      schema: {
        type: "object",
        properties: {
          command: { type: "string", description: "The bash command or script to run." },
          cwd: { type: "string", description: "Working directory (default: the workspace)." },
          timeout_ms: { type: "integer", minimum: 1, maximum: MAX_TIMEOUT_MS, description: "Kill it after this long (default 120000)." },
        },
        required: ["command"],
        additionalProperties: false,
      },
      input: (raw: unknown) => {
        const input = object(raw);
        return {
          command: str(input, "command")!,
          cwd: str(input, "cwd", { optional: true }) ?? ".",
          timeout: int(input, "timeout_ms", 1, MAX_TIMEOUT_MS) ?? DEFAULT_TIMEOUT_MS,
        };
      },
      async process({ command, cwd, timeout }: { command: string; cwd: string; timeout: number }, ctx: any) {
        const r = await ctx.exec(command, { cwd, timeout });
        return {
          exit_code: r.code,
          signal: r.signal,
          timed_out: r.timedOut,
          stdout: r.stdout,
          stderr: r.stderr,
          truncated: r.truncated,
          changes: r.changes,
          changes_complete: r.changesComplete,
        };
      },
    },

    read: {
      description: "Read a text file. Lines come back numbered (like `cat -n`); use offset and limit for long files.",
      schema: {
        type: "object",
        properties: {
          path: { type: "string", description: "File to read (relative to the workspace or absolute)." },
          offset: { type: "integer", minimum: 1, description: "First line to return (1-based)." },
          limit: { type: "integer", minimum: 1, description: `How many lines (default ${DEFAULT_READ_LINES}).` },
        },
        required: ["path"],
        additionalProperties: false,
      },
      input: (raw: unknown) => {
        const input = object(raw);
        return {
          path: str(input, "path")!,
          offset: int(input, "offset", 1, Number.MAX_SAFE_INTEGER) ?? 1,
          limit: int(input, "limit", 1, Number.MAX_SAFE_INTEGER) ?? DEFAULT_READ_LINES,
        };
      },
      async process({ path: file, offset, limit }: { path: string; offset: number; limit: number }) {
        const text = await fs.readFile(file, "utf8");
        const lines = text.split("\n");
        if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
        const slice = lines.slice(offset - 1, offset - 1 + limit);
        const content = slice.map((line, i) => `${String(offset + i).padStart(6)}\t${line}`).join("\n");
        return { path: path.resolve(file), content, total_lines: lines.length, truncated: offset - 1 + slice.length < lines.length };
      },
    },

    edit: {
      description:
        "Replace an exact string in a file. old_string must occur exactly once unless replace_all is set; " +
        "include enough surrounding text to make it unique.",
      schema: {
        type: "object",
        properties: {
          path: { type: "string" },
          old_string: { type: "string", description: "Exact text to replace." },
          new_string: { type: "string", description: "Replacement text." },
          replace_all: { type: "boolean", description: "Replace every occurrence." },
        },
        required: ["path", "old_string", "new_string"],
        additionalProperties: false,
      },
      input: (raw: unknown) => {
        const input = object(raw);
        const edit = {
          path: str(input, "path")!,
          old: str(input, "old_string")!,
          new: str(input, "new_string", { allowEmpty: true })!,
          all: bool(input, "replace_all"),
        };
        if (edit.old === edit.new) fail("old_string and new_string are the same");
        return edit;
      },
      async process(edit: { path: string; old: string; new: string; all: boolean }) {
        if ((await kindOf(edit.path)) !== "file") fail(`${edit.path}: no such file (use create for new files)`);
        const text = await fs.readFile(edit.path, "utf8");
        const found = count(text, edit.old);
        if (found === 0) fail(`old_string was not found in ${edit.path}`);
        if (found > 1 && !edit.all) fail(`old_string occurs ${found} times in ${edit.path}; add context or set replace_all`);
        const updated = edit.all ? text.split(edit.old).join(edit.new) : text.replace(edit.old, () => edit.new);
        await fs.writeFile(edit.path, updated);
        return { path: path.resolve(edit.path), replacements: edit.all ? found : 1 };
      },
    },

    create: {
      description: "Create a file with the given content (parent directories too). Refuses to replace an existing file unless overwrite is set.",
      schema: {
        type: "object",
        properties: {
          path: { type: "string" },
          content: { type: "string" },
          overwrite: { type: "boolean", description: "Replace the file if it exists." },
        },
        required: ["path", "content"],
        additionalProperties: false,
      },
      input: (raw: unknown) => {
        const input = object(raw);
        return { path: str(input, "path")!, content: str(input, "content", { allowEmpty: true })!, overwrite: bool(input, "overwrite") };
      },
      async process({ path: file, content, overwrite }: { path: string; content: string; overwrite: boolean }) {
        const existing = await kindOf(file);
        if (existing === "dir") fail(`${file} is a directory`);
        if (existing === "file" && !overwrite) fail(`${file} already exists (set overwrite to replace it, or use edit)`);
        const parent = path.dirname(path.resolve(file));
        if ((await kindOf(parent)) === null) await fs.mkdir(parent, { recursive: true });
        await fs.writeFile(file, content);
        return { path: path.resolve(file), bytes: Buffer.byteLength(content), overwritten: existing === "file" };
      },
    },

    delete: {
      description: "Delete a file, or a directory with recursive set.",
      schema: {
        type: "object",
        properties: {
          path: { type: "string" },
          recursive: { type: "boolean", description: "Required to delete a directory and everything in it." },
        },
        required: ["path"],
        additionalProperties: false,
      },
      input: (raw: unknown) => {
        const input = object(raw);
        return { path: str(input, "path")!, recursive: bool(input, "recursive") };
      },
      async process({ path: file, recursive }: { path: string; recursive: boolean }) {
        const kind = await kindOf(file);
        if (kind === null) fail(`${file}: no such file or directory`);
        if (kind === "dir" && !recursive) fail(`${file} is a directory (set recursive to delete it)`);
        await fs.rm(file, { recursive });
        return { path: path.resolve(file), kind };
      },
    },
  },
});
