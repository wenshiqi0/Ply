import plugins from "wendy:plugins";
// Current MiMo IDs are explicit here: older pi-ai catalogs still list the
// retired V2.5 models. Do not silently substitute those model IDs.
// Sources: https://mimo.mi.com/docs/en-US/quick-start/model
//          https://mimo.mi.com/docs/en-US/pricing
const endpoints: Record<string, string> = {
  "xiaomi": "https://api.xiaomimimo.com/v1",
  "xiaomi-token-plan-cn": "https://token-plan-cn.xiaomimimo.com/v1",
  "xiaomi-token-plan-sgp": "https://token-plan-sgp.xiaomimimo.com/v1",
  "xiaomi-token-plan-ams": "https://token-plan-ams.xiaomimimo.com/v1",
};

// USD per million tokens for the public pay-as-you-go API, checked 2026-10-24.
const models: Record<string, { name: string; input: number; output: number; cacheRead: number }> = {
  "mimo-v2.6-pro": { name: "MiMo-V2.6-Pro", input: 0.435, output: 0.87, cacheRead: 0.0036 },
  "mimo-v2.6-flash": { name: "MiMo-V2.6-Flash", input: 0.14, output: 0.28, cacheRead: 0.0028 },
  "mimo-v2.6-pro-ultraspeed": { name: "MiMo-V2.6-Pro-UltraSpeed", input: 4.35, output: 8.7, cacheRead: 0.036 },
};

/** Select a MiMo V2.6 model for the agent, using MIMO_API_KEY on the host. */
export async function onLoad() {
  await plugins.context.loaded;
  const id = process.env.WENDY_MODEL ?? "mimo-v2.6-pro";
  const provider = process.env.WENDY_MIMO_PROVIDER ?? "xiaomi-token-plan-cn";
  if (!Object.hasOwn(models, id)) throw new Error(`unsupported MiMo model: ${id}`);
  if (!Object.hasOwn(endpoints, provider)) throw new Error(`unsupported MiMo provider: ${provider}`);
  if (provider !== "xiaomi" && id === "mimo-v2.6-pro-ultraspeed") {
    throw new Error("mimo-v2.6-pro-ultraspeed requires the pay-as-you-go xiaomi provider");
  }
  const selected = models[id];
  // Token Plan uses a subscription/credit quota, not these public API rates.
  // pi-ai requires numeric cost metadata; zero here is not a free-price claim.
  // Wendy reports tokens and does not expose SDK cost estimates as billing.
  const cost = provider === "xiaomi"
    ? { input: selected.input, output: selected.output, cacheRead: selected.cacheRead, cacheWrite: 0 }
    : { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
  await ctx.useModel({
    provider,
    model: id,
    apiKeyEnv: "MIMO_API_KEY",
    apiKeyHeader: "api-key",
    reasoningEffort: "high",
    definition: {
      id, provider, name: selected.name, api: "openai-completions",
      baseUrl: endpoints[provider], reasoning: true, input: ["text", "image"],
      contextWindow: 1_048_576, maxTokens: 131_072, cost,
      compat: {
        supportsStore: false,
        supportsDeveloperRole: false,
        supportsReasoningEffort: false,
        requiresReasoningContentOnAssistantMessages: true,
        thinkingFormat: "deepseek",
      },
    },
  });
}
