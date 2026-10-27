declare module "ply:sh" {
  export interface Result {
    exit_code: number | null;
    signal: string | null;
    timed_out: boolean;
    stdout: string;
    stderr: string;
  }
  /** `env` is the command's whole environment; it defaults to `process.env`. */
  export function exec(request: { command: string; cwd?: string; timeout_ms?: number; env?: Record<string, string | undefined> }): Promise<Result>;
}

/** Plugin-owned, cross-realm events. Declare/subscribe during onLoad. */
