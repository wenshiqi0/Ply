import { context } from "ply:plugins";
import { endpoints, models } from "./models";
import { profileFor } from "./profiles";

/** Select a MiMo V2.6 model for the agent, using MIMO_API_KEY on the host. */
export async function onLoad() {
  const id = process.env.PLY_MODEL ?? "mimo-v2.6-pro";
  const provider = process.env.PLY_MIMO_PROVIDER ?? "xiaomi-token-plan-cn";
  if (!Object.hasOwn(models, id)) throw new Error(`unsupported MiMo model: ${id}`);
  if (!Object.hasOwn(endpoints, provider)) throw new Error(`unsupported MiMo provider: ${provider}`);
  if (provider !== "xiaomi" && id === "mimo-v2.6-pro-ultraspeed") {
    throw new Error("mimo-v2.6-pro-ultraspeed requires the pay-as-you-go xiaomi provider");
  }
  await context.useModel(profileFor(id, provider));
  return { modelProfiles: () => Object.keys(models).filter(model => provider === "xiaomi" || model !== "mimo-v2.6-pro-ultraspeed").map(model => profileFor(model, provider)) };
}
