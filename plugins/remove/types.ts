

export type RemoveInput = {
  path: string;
  /** Required to remove a directory. */
  recursive?: boolean;
};

export type RemoveResult = { path: string; kind: "file" | "dir" };
