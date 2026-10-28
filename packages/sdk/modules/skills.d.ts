declare module "ply:skills" {
  export interface SkillInfo { name: string; description: string; path: string; directory: string }
  export interface SkillCatalog { skills: SkillInfo[]; warnings: { path: string; message: string }[] }
  export interface SkillDocument {
    name: string; description: string; body: string;
    license: string | null; compatibility: string | null;
    metadata: Record<string, unknown> | null; allowed_tools: string | null;
  }
  /** YAML frontmatter plus Markdown body; data only, at most 64 KiB. */
  export function parse(source: string): SkillDocument;
}
