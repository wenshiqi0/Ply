import plugins from "wendy:plugins";
import * as events from "wendy:events";

/** Adapter template: replace this pending response with your environment's UI/service. */
export async function onLoad() {
  await plugins.approval.loaded;
  const pending = async (event: events.Event<WendyApprovalRequest | WendyToolApprovalRequest>): Promise<WendyApprovalResponse> => {
    await fiber.record({ type: "environment_approval_needed", request_id: event.data.id, tool: event.data.tool, cwd: event.data.cwd,
      ...("command" in event.data ? { command: event.data.command } : { input: event.data.input }) });
    // An actual adapter must await an authenticated user's/service's answer.
    // Do not approve based on model output or command-supplied flags.
    return { request_id: event.data.id, decision: "pending", reason: "approval environment is not connected" };
  };
  await events.on("approval.request", pending);
  await events.on("approval.toolRequest", pending);
}
