import path from "node:path";
import type { SkillOptions } from "./types";
import { context } from "ply:plugins";
import { discoverSkills, paths, skillContext } from "./discovery";

/** Discover standard SKILL.md files and contribute their metadata to model context. */
export async function onLoad(options: SkillOptions = {}) {
  if (!options || typeof options !== "object" || Array.isArray(options) || Object.keys(options).some(k => k !== "paths" && k !== "directory")) throw new Error("skill options must contain only paths and directory");
  if (options.directory !== undefined && (typeof options.directory !== "string" || !options.directory.trim())) throw new Error("skill directory must be a non-empty path");
  const directory = path.resolve(options.directory ?? process.env.PLY_LAUNCH_DIRECTORY ?? process.cwd());
  const configured = { paths: paths(options.paths ?? [], "skill paths", directory),
    auto: [...new Set([path.join(directory, ".agents/skills"), path.join(directory, "skills"), path.join(directory, "SKILL.md"),
      ...(process.env.PLY_USER_SKILLS_DIRECTORY ? [process.env.PLY_USER_SKILLS_DIRECTORY] : [])])] };
  const catalog = await discoverSkills(configured);
  await context.set({ source: "skill", type: context.Type.Extensions, key: "skills", content: catalog.skills.length ? skillContext(catalog) : null });
  return { catalog };
}
