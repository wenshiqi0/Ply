import plugins from "ply:plugins";
import * as events from "ply:events";

type BashRequest = { command: string; cwd: string; timeout_ms: number };

/** Example before hook. The marker is a demo rule, not a shell security policy. */
export async function onLoad() {
  await plugins.bash.loaded;
  await events.on<BashRequest>("bash.beforeExecute", async event => {
    await fiber.record({ type: "bash_before", source: event.source, command: event.data.command, cwd: event.data.cwd });
    if (event.data.command.includes("__deny_demo__")) {
      return { allow: false, reason: "demo hook rejected __deny_demo__" };
    }
    return { allow: true };
  });
}
