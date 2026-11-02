import type { AgentInput, AgentResult } from "./types";
import { context } from "ply:plugins";
import { declareAudit } from "./audit";
import { onHandle as runAgent } from "./run";
import { hostChat, modelCatalog } from "./host";
import { hostState, hostApprove, listenHost } from "./host-approval";

/** Offer this plugin to the model so agents can start child agents. */
export async function onLoad() {
  await declareAudit();
  await context.registerTool("agent");
  await listenHost();
  return { hostChat, modelCatalog, hostState, hostApprove };
}

/** Run a task with bounded tool/delegation grants, or resume a recovery ID. */
export async function onHandle(input: AgentInput): Promise<AgentResult> { return runAgent(input); }
