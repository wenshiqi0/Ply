

export type EditInput = {
  path: string;
  /** Exact text to replace. */
  old_string: string;
  /** Replacement text. */
  new_string: string;
  /** Replace every occurrence. */
  replace_all?: boolean;
};

export type EditResult = { path: string; replacements: number };
