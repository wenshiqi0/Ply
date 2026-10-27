import type { Context as PiContext, Model as PiModel, Api } from "@earendil-works/pi-ai";
import type { ModelLimits, ModelProfile } from "ply:plugins";
import type { Context, Message } from "./types";

// The model connection: pi-ai from npm, over the runtime's fetch (RFC 0013).
// Loaded on the first request, so starting the agent does not parse the SDK.
let loading: ReturnType<typeof load> | undefined;

async function load() {
  const [{ createModels }, deepseek, xiaomi, cn, sgp, ams] = await Promise.all([
    import("@earendil-works/pi-ai"),
    import("@earendil-works/pi-ai/providers/deepseek"),
    import("@earendil-works/pi-ai/providers/xiaomi"),
    import("@earendil-works/pi-ai/providers/xiaomi-token-plan-cn"),
    import("@earendil-works/pi-ai/providers/xiaomi-token-plan-sgp"),
    import("@earendil-works/pi-ai/providers/xiaomi-token-plan-ams"),
  ]);
  // The key comes from the profile's variable only: no ambient lookup of
  // other variables or credential files.
  const models = createModels({ authContext: { env: async () => undefined, fileExists: async () => false } });
  for (const provider of [deepseek.deepseekProvider(), xiaomi.xiaomiProvider(), cn.xiaomiTokenPlanCnProvider(),
    sgp.xiaomiTokenPlanSgpProvider(), ams.xiaomiTokenPlanAmsProvider()]) {
    models.setProvider(provider);
  }
  return models;
}

async function resolve(profile: ModelProfile) {
  const models = await (loading ??= load());
  if (!models.getProvider(profile.provider)) throw new Error("unsupported model provider: " + profile.provider);
  const model = (profile.definition as PiModel<Api> | undefined) ?? models.getModel(profile.provider, profile.model);
  if (!model) throw new Error(`unknown model ${profile.provider}/${profile.model}`);
  if (model.provider !== profile.provider || model.id !== profile.model || model.api !== "openai-completions") {
    throw new Error("model definition does not match its provider and the openai-completions API");
  }
  return { models, model };
}

export async function request(input: { operation: "describe"; profile: ModelProfile }): Promise<ModelLimits>;
export async function request(input: {
  operation: "complete"; profile: ModelProfile; context: Context; purpose: "task" | "compaction"; maxTokens: number;
}): Promise<Message>;
export async function request(input: {
  operation: "describe" | "complete"; profile: ModelProfile; context?: Context; purpose?: "task" | "compaction"; maxTokens?: number;
}): Promise<ModelLimits | Message> {
  const { profile } = input;
  const { models, model } = await resolve(profile);
  if (input.operation === "describe") return { contextWindow: model.contextWindow, maxTokens: model.maxTokens };
  const maxTokens = input.maxTokens ?? 0;
  if (!Number.isSafeInteger(maxTokens) || maxTokens < 1) throw new Error("maxTokens must be a positive safe integer");
  if (!profile.apiKeyEnv) throw new Error(`the ${profile.provider} profile names no apiKeyEnv`);
  const apiKey = process.env[profile.apiKeyEnv]?.trim();
  if (!apiKey) throw new Error("API key environment variable is not set: " + profile.apiKeyEnv);
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 600_000);
  try {
    return await models.complete(model, input.context as PiContext, {
      apiKey,
      ...(profile.apiKeyHeader ? { headers: { [profile.apiKeyHeader]: apiKey } } : {}),
      ...(input.purpose !== "compaction" && profile.reasoningEffort ? { reasoningEffort: profile.reasoningEffort } : {}),
      maxTokens, signal: controller.signal, timeoutMs: 600_000,
    });
  } finally {
    clearTimeout(timeout);
    controller.abort();
  }
}
