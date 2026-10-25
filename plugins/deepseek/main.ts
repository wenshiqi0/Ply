import plugins from "wendy:plugins";
/**
 * DeepSeek for this machine: tells the context which model the agent should
 * use and which environment variable holds its key. The agent makes the
 * calls itself.
 */
export async function onLoad() {
  await plugins.context.loaded;
  await ctx.useModel({ provider: "deepseek", model: process.env.WENDY_MODEL ?? "deepseek-v4-flash", apiKeyEnv: "DEEPSEEK_API_KEY" });
}
