

export type CreateInput = {
  path: string;
  content: string;
  /** Replace the file if it exists. */
  overwrite?: boolean;
};

export type CreateResult = { path: string; bytes: number; overwritten: boolean };
