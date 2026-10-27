import type { RunUsage } from "./types";

// Agents of this plugin share a realm. A parent hands each delegated agent
// call a one-time token for its own run, so the child inherits its limits,
// context and usage chain without knowing who called it.
export const delegations = new Map<string, RunUsage>();
