import * as events from "ply:events";

/** Adapter template: replace this pending response with your environment's UI/service. */
export async function onLoad() {
  const pending = async (event: events.Event<PlyApprovalRequest | PlyToolApprovalRequest>): Promise<PlyApprovalResponse> => {
    console.log(`approval needed: ${event.data.id} ${event.data.tool} in ${event.data.cwd}`);
    // An actual adapter must await an authenticated user's/service's answer.
    // Do not approve based on model output or command-supplied flags.
    return { request_id: event.data.id, decision: "pending", reason: "approval environment is not connected" };
  };
  await events.on("approval.request", pending);
  await events.on("approval.toolRequest", pending);
}
