import { execFile } from "node:child_process";

/**
 * Run a command with the system bash, in the workspace by default. Returns
 * the exit code and what it printed; processes it leaves behind are stopped
 * when it exits.
 */
export async function handle(input: {
  /** The bash command or script to run. */
  command: string;
  /** Working directory (default: the workspace). */
  cwd?: string;
  /** Stop it after this many milliseconds (default 120000). */
  timeout_ms?: number;
}): Promise<{
  exit_code: number | null;
  signal: string | null;
  timed_out: boolean;
  stdout: string;
  stderr: string;
}> {
  if (typeof input?.command !== "string" || input.command === "") throw new Error("'command' must be a non-empty string");
  const options = { cwd: input.cwd ?? ".", timeout: input.timeout_ms ?? 120_000 };
  return await new Promise((resolve, reject) => {
    execFile("/bin/bash", ["-c", input.command], options, (err: any, stdout: string, stderr: string) => {
      // A string code means it could not start at all (e.g. ENOTSUP).
      if (err && typeof err.code === "string") return reject(err);
      resolve({ exit_code: err ? err.code : 0, signal: err?.signal ?? null, timed_out: Boolean(err?.killed), stdout, stderr });
    });
  });
}
