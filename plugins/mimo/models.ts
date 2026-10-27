

// Current MiMo IDs are explicit here: older pi-ai catalogs still list the
// retired V2.5 models. Do not silently substitute those model IDs.
// Sources: https://mimo.mi.com/docs/en-US/quick-start/model
//          https://mimo.mi.com/docs/en-US/pricing
export const endpoints: Record<string, string> = {
  "xiaomi": "https://api.xiaomimimo.com/v1",
  "xiaomi-token-plan-cn": "https://token-plan-cn.xiaomimimo.com/v1",
  "xiaomi-token-plan-sgp": "https://token-plan-sgp.xiaomimimo.com/v1",
  "xiaomi-token-plan-ams": "https://token-plan-ams.xiaomimimo.com/v1",
};

// USD per million tokens for the public pay-as-you-go API, checked 2026-10-24.
export const models: Record<string, { name: string; input: number; output: number; cacheRead: number }> = {
  "mimo-v2.6-pro": { name: "MiMo-V2.6-Pro", input: 0.435, output: 0.87, cacheRead: 0.0036 },
  "mimo-v2.6-flash": { name: "MiMo-V2.6-Flash", input: 0.14, output: 0.28, cacheRead: 0.0028 },
  "mimo-v2.6-pro-ultraspeed": { name: "MiMo-V2.6-Pro-UltraSpeed", input: 4.35, output: 8.7, cacheRead: 0.036 },
};
