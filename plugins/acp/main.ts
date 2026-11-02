import * as commands from "ply:commands";
import { agent, approval } from "ply:plugins";
import { prepareServer } from "./server";
import type { Options } from "./types";

/** Agent Client Protocol host adapter; never registered as a model tool. */
export async function onLoad(options: Options) {
  // Named imports establish readiness without taking ownership of agent state.
  void agent; void approval;
  const handler = await prepareServer(options);
  await commands.register({ name: "acp", description: "Serve ACP v1 agent sessions over stdio", stdio: "inherit", handler });
}
