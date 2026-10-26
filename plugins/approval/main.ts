import plugins from "ply:plugins";
import * as events from "ply:events";

let requestEvent: string;
let toolEvent: string;
let timeout: number;
let mode: "risk" | "ask" | "bypass";
let nextToolRequest = 0;
const stats = { requested: 0, approved: 0, rejected: 0, pending: 0, bypassed: 0 };

type Input = { id: string; tool: "bash"; command: string; cwd: string; assessments: unknown[] };
type Response = { request_id: string; decision: "approved" | "rejected" | "pending"; reason?: string; reference?: string; approver?: string };

async function bypassDecision(request: { id: string; [key: string]: unknown }) {
  stats.bypassed++;
  const result = { status: "approved" as const, request, responses: [], reason: "approval_bypassed" };
  // Host-selected authorization, never a fabricated user/provider response.
  await fiber.record({ type: "approval_bypassed", ...result });
  return result;
}

async function requestDecision(event: string, request: { id: string; [key: string]: unknown }) {
  stats.requested++;
  await fiber.record({ type: "approval_requested", request });
  let responses: Response[] = [];
  let status: "approved" | "rejected" | "pending" = "pending";
  let reason = "approval_provider_missing";
  try {
    const results = await events.emit<unknown, unknown>(event, request, { timeout_ms: timeout });
    if (results.length) {
      const valid = results.every(value => value && typeof value === "object" && (value as any).request_id === request.id && ["approved", "rejected", "pending"].includes((value as any).decision)
        && ["reason", "reference", "approver"].every(key => (value as any)[key] === undefined || typeof (value as any)[key] === "string"));
      if (!valid) reason = "invalid_approval_response";
      else {
        responses = results as Response[];
        status = responses.some(response => response.decision === "rejected") ? "rejected"
          : responses.every(response => response.decision === "approved") ? "approved" : "pending";
        reason = status === "approved" ? "explicitly_approved" : status === "rejected" ? "explicitly_rejected" : "approval_pending";
      }
    }
  } catch { reason = "approval_provider_unavailable"; }
  stats[status]++;
  const result = { status, request, responses, reason };
  await fiber.record({ type: "approval_result", ...result });
  return result;
}

/** Host-selected approval policy; environment plugins supply user/service decisions. */
export async function onLoad() {
  timeout = Number(process.env.PLY_APPROVAL_TIMEOUT_MS ?? 300000);
  if (!Number.isSafeInteger(timeout) || timeout < 1 || timeout > 86400000) throw new Error("PLY_APPROVAL_TIMEOUT_MS must be an integer in 1..86400000");
  const configured = process.env.PLY_APPROVAL_MODE ?? "risk";
  if (configured !== "risk" && configured !== "ask" && configured !== "bypass") throw new Error("PLY_APPROVAL_MODE must be risk, ask or bypass");
  mode = configured;
  requestEvent = await events.define("request");
  toolEvent = await events.define("toolRequest");
  if (mode === "ask" || mode === "bypass") {
    await plugins.bash.loaded;
    await events.on("bash.beforeExecute", () => ({ allow: false, requires_approval: true, decision: "review", reason: mode === "bypass" ? "host selected bypass authorization for this bash call" : "approval policy requires confirmation for this bash call" }));
  }
  return {
    async requestCommandApproval(input: Input) {
      const current = await fiber.info();
      if (!input || input.id !== current.id + ":approval" || input.tool !== "bash" || typeof input.command !== "string" || typeof input.cwd !== "string" || !Array.isArray(input.assessments)) throw new Error("invalid command approval request");
      const request = { schema_version: 1, id: input.id, tool: input.tool, command: input.command, cwd: input.cwd,
        assessments: input.assessments, source: { fiber: current.id, parent: current.parent }, requested_at: Date.now() };
      return mode === "bypass" ? await bypassDecision(request) : await requestDecision(requestEvent, request);
    },
    async authorizeToolCall(input: { tool: string; input: unknown; cwd: string }) {
      if (!input || typeof input.tool !== "string" || !/^[a-zA-Z0-9_-]{1,64}$/.test(input.tool) || input.tool === "bash" || typeof input.cwd !== "string") throw new Error("invalid model tool approval request");
      if (mode === "risk") return { status: "not_required" };
      const current = await fiber.info();
      const request = { schema_version: 1, id: current.id + ":tool-approval:" + (++nextToolRequest), tool: input.tool,
        input: input.input ?? null, cwd: input.cwd, source: { fiber: current.id, parent: current.parent }, requested_at: Date.now() };
      return mode === "bypass" ? await bypassDecision(request) : await requestDecision(toolEvent, request);
    },
    approvalStatus: () => ({ ...stats, mode, timeout_ms: timeout }),
  };
}
