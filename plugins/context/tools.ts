import plugins from "ply:plugins";
import type { ToolDef } from "./types";
import { offered } from "./state";

export function toolList(allowed?: string[]): ToolDef[] {
  return [...offered].sort().filter((name) => (!allowed || allowed.includes(name)) && plugins[name]?.parameters)
    .map((name) => ({ name, description: plugins[name].description, parameters: plugins[name].parameters }));
}
