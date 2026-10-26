import { context } from "ply:plugins";
/**
 * DeepSeek for this machine: tells the context which model the agent should
 * use and which environment variable holds its key. The agent makes the
 * calls itself.
 */
export async function onLoad() {
  await context.loaded;
  await context.useModel({ provider: "deepseek", model: process.env.PLY_MODEL ?? "deepseek-v4-flash", apiKeyEnv: "DEEPSEEK_API_KEY" });
}
