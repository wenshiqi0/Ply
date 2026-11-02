import type { ModelSelection } from "./types";
import { endpoints, models } from "./models";

export function profileFor(id: string, provider: string): ModelSelection {
  const selected = models[id];
  // Token Plan uses a subscription/credit quota, not these public API rates.
  // pi-ai requires numeric cost metadata; zero here is not a free-price claim.
  // Ply reports tokens and does not expose SDK cost estimates as billing.
  const cost = provider === "xiaomi"
    ? { input: selected.input, output: selected.output, cacheRead: selected.cacheRead, cacheWrite: 0 }
    : { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
  return {
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
  } satisfies ModelSelection;
}
