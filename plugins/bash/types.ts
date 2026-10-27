

export type Decision = boolean | null | { allow?: boolean; reason?: string; requires_approval?: boolean };

export type BashInput = {
  /** The script interpreted by Ply sh. */
  command: string;
  /** Working directory (default: the workspace). */
  cwd?: string;
  /** Inspection + execution deadline; approval waiting has its own deadline. */
  timeout_ms?: number;
};

export type BashResult = {
  exit_code: number | null;
  signal: string | null;
  timed_out: boolean;
  stdout: string;
  stderr: string;
  status?: "approval_required";
  approval?: unknown;
};
