import * as events from "ply:events";

type Request = PlyApprovalRequest | PlyToolApprovalRequest;
type Decision = { request_id: string; decision: "approved" | "rejected" | "pending"; reason?: string };
const pending = new Map<string, { request: Request; resolve: (decision: Decision) => void }>();
let active = false;
let listening = false;

export function activeHost() {
  if (active) throw new Error("host conversation already running");
  active = true;
}

export function finishHost() {
  active = false;
  for (const [id, entry] of pending) entry.resolve({ request_id: id, decision: "rejected", reason: "host turn ended" });
  pending.clear();
}

export function hostState() { return { pending: [...pending.values()].map(entry => entry.request) }; }

export function hostApprove(input: { id: string; accepted: boolean }) {
  const entry = pending.get(input?.id);
  if (!entry || typeof input.accepted !== "boolean") throw new Error("approval expired");
  pending.delete(input.id);
  entry.resolve({ request_id: input.id, decision: input.accepted ? "approved" : "rejected", reason: "explicit host decision" });
  return {};
}

export async function listenHost() {
  if (process.env.PLY_HOST_CHAT !== "1") return;
  if (listening) return;
  listening = true;
  const review = async (event: events.Event<Request>): Promise<Decision> => {
    if (!active) return { request_id: event.data.id, decision: "pending", reason: "no active host conversation" };
    try {
      return await new Promise<Decision>(resolve => { pending.set(event.data.id, { request: event.data, resolve }); });
    } finally { pending.delete(event.data.id); }
  };
  await events.on("approval.request", review);
  await events.on("approval.toolRequest", review);
}
