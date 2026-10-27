import type { ModelSelection } from "./types";
import { context } from "ply:plugins";
import { endpoints, models } from "./models";

/** Select a MiMo V2.6 model for the agent, using MIMO_API_KEY on the host. */
export async function onLoad() {
  const id = process.env.PLY_MODEL ?? "mimo-v2.6-pro";
  const provider = process.env.PLY_MIMO_PROVIDER ?? "xiaomi-token-plan-cn";
  if (!Object.hasOwn(models, id)) throw new Error(`unsupported MiMo model: ${id}`);
  if (!Object.hasOwn(endpoints, provider)) throw new Error(`unsupported MiMo provider: ${provider}`);
  if (provider !== "xiaomi" && id === "mimo-v2.6-pro-ultraspeed") {
    throw new Error("mimo-v2.6-pro-ultraspeed requires the pay-as-you-go xiaomi provider");
  }
  const selected = models[id];
  // Token Plan uses a subscription/credit quota, not these public API rates.
  // pi-ai requires numeric cost metadata; zero here is not a free-price claim.
  // Ply reports tokens and does not expose SDK cost estimates as billing.
  const cost = provider === "xiaomi"
    ? { input: selected.input, output: selected.output, cacheRead: selected.cacheRead, cacheWrite: 0 }
    : { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
  const profile: ModelSelection = {
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
  };
  await context.useModel(profile);
}
