

export type ReadInput = {
  /** File to read, relative to the workspace or an absolute mounted path. */
  path: string;
  /** First line to read, starting at 1 (default 1). */
  offset?: number;
  /** Maximum number of lines to return (default 200). */
  limit?: number;
};

export type ReadResult = { path: string; content: string; offset: number; lines: number; total_lines: number; has_more: boolean };
