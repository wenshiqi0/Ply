declare module "ply:sh" {
  export interface Result {
    exit_code: number | null;
    signal: string | null;
    timed_out: boolean;
    stdout: string;
    stderr: string;
  }
  export function exec(request: { command: string; cwd?: string; timeout_ms?: number }): Promise<Result>;
}

/** Plugin-owned, cross-realm events. Declare/subscribe during onLoad. */
