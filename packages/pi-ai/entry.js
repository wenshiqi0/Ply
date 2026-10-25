import { createModels } from "@earendil-works/pi-ai";
import { deepseekProvider } from "@earendil-works/pi-ai/providers/deepseek";
import { xiaomiProvider } from "@earendil-works/pi-ai/providers/xiaomi";
import { xiaomiTokenPlanCnProvider } from "@earendil-works/pi-ai/providers/xiaomi-token-plan-cn";
import { xiaomiTokenPlanSgpProvider } from "@earendil-works/pi-ai/providers/xiaomi-token-plan-sgp";
import { xiaomiTokenPlanAmsProvider } from "@earendil-works/pi-ai/providers/xiaomi-token-plan-ams";
import { createWeb } from "./web.js";
export { createWeb };

export function createRuntime(host, web) {
  const models = createModels({ authContext: { env: async () => undefined, fileExists: async () => false } });
  for (const provider of [deepseekProvider(), xiaomiProvider(), xiaomiTokenPlanCnProvider(), xiaomiTokenPlanSgpProvider(), xiaomiTokenPlanAmsProvider()]) {
    models.setProvider(provider);
  }
  async function request({ operation, profile, context, purpose, maxTokens }) {
    if (!profile || !models.getProvider(profile.provider)) throw new Error("unsupported embedded model provider: " + profile?.provider);
    const model = profile.definition ?? models.getModel(profile.provider, profile.model);
    if (!model) throw new Error("unknown model " + profile.provider + "/" + profile.model);
    if (model.provider !== profile.provider || model.id !== profile.model || model.api !== "openai-completions") {
      throw new Error("model definition does not match the embedded provider/API");
    }
    if (operation === "describe") return { contextWindow: model.contextWindow, maxTokens: model.maxTokens };
    if (operation !== "complete") throw new Error("unknown model operation");
    if (!Number.isSafeInteger(maxTokens) || maxTokens < 1) throw new Error("maxTokens must be a positive safe integer");
    // Host-only environment lookup; never add a key to process.env, the
    // profile, context, a public result, or a usage record.
    const apiKey = host.credentials(profile);
    const controller = new web.AbortController();
    const timeout = setTimeout(() => controller.abort(), 600_000);
    try {
      return await models.complete(model, context, {
        apiKey,
        ...(profile.apiKeyHeader ? { headers: { [profile.apiKeyHeader]: apiKey } } : {}),
        ...(purpose !== "compaction" && profile.reasoningEffort ? { reasoningEffort: profile.reasoningEffort } : {}),
        maxTokens, signal: controller.signal, timeoutMs: 600_000,
      });
    } finally {
      clearTimeout(timeout);
      controller.abort();
    }
  }
  return { web, request };
}
