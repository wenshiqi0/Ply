import * as events from "ply:events";

type BashRequest = { command: string; cwd: string; timeout_ms: number };

/** Example before hook. The marker is a demo rule, not a shell security policy. */
export async function onLoad() {
  await events.on<BashRequest>("bash.beforeExecute", async event => {
    console.log(`bash-guard: ${event.data.cwd}: ${event.data.command}`);
    if (event.data.command.includes("__deny_demo__")) {
      return { allow: false, reason: "demo hook rejected __deny_demo__" };
    }
    return { allow: true };
  });
}
