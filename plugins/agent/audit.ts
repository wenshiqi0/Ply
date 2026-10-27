import * as events from "ply:events";

// The agent's audit trail: plugin events the kernel records with the call
// that emitted them. Nothing needs to listen.
const names = { model_call_started: "", model_call: "", context_compaction: "", policy: "", checkpoint: "", report: "" };

/** Declare the audit events; onLoad only. */
export async function declareAudit() {
  for (const local of Object.keys(names) as (keyof typeof names)[]) names[local] = await events.define(local);
}

/** Record an audit event. A call that was cancelled and is cleaning up can no longer emit; that never changes its outcome. */
export async function audit(event: keyof typeof names, data: Record<string, unknown>) {
  try { await events.emit(names[event], data); } catch { /* auditing is best-effort once the call has exited */ }
}
