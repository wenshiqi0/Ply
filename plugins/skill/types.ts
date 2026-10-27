

export type SkillSources = { paths: string[]; auto: string[] };

export type SkillOptions = {
  /** Extra mounted SKILL.md files, skill directories or collections. */
  paths?: string[];
  /** Discovery base (default: command launch directory, or embedded host cwd). */
  directory?: string;
};
