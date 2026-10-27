import type { Recovery } from "./types";
import { integer } from "./policy";
import { sectionNames } from "./report";

export function recovery(raw: unknown): Recovery {
  const value = raw as Recovery | null;
  if (!value || value.format !== 4) throw new Error("unsupported recovery format: expected 4; older checkpoints cannot be resumed; start a new task");
  if ( !["ready", "tools_pending"].includes(value.phase) || value.cwd !== process.cwd() ||
      !value.profile || typeof value.profile.provider !== "string" || typeof value.profile.model !== "string" ||
      !value.policy || !Array.isArray(value.policy.allowed_tools)) throw new Error("invalid recovery record or workspace mismatch");
  integer(value.next_turn, "checkpoint next_turn", 1);
  integer(value.tool_calls, "checkpoint tool_calls");
  integer(value.policy.max_delegation_depth, "checkpoint delegation depth");
  integer(value.policy.max_children, "checkpoint child limit");
  integer(value.children_started, "checkpoint children_started");
  if (!value.report || typeof value.report !== "object") throw new Error("invalid checkpoint report state");
  value.report.required_sections = sectionNames(value.report.required_sections, "checkpoint report sections");
  integer(value.report.rejected_reports, "checkpoint rejected_reports");
  if (value.policy.allowed_tools.some((name: unknown) => typeof name !== "string" || !/^[a-zA-Z0-9_-]{1,64}$/.test(name))) {
    throw new Error("invalid checkpoint tool permissions");
  }
  return value as Recovery;
}
