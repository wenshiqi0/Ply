import plugins, { context, type ModelProfile } from "ply:plugins";
import { onHandle } from "./run";
import { nativeProfiles, describeProfile } from "./pi";
import { activeHost, finishHost } from "./host-approval";

export type HostImage = { type: "image"; data: string; mimeType: string };
export type HostConversation = { snapshot?: unknown; images?: HostImage[] };
export type HostModel = { id: string; name: string; input: string[]; efforts: string[]; defaultEffort: string; profile: ModelProfile };

/** Native facts only; this export is not an agent-visible tool. */
export async function modelCatalog(): Promise<HostModel[]> {
  const active = await context.currentModel();
  if (!active) throw new Error("model integration unavailable");
  const catalog = plugins["mimo"].modelProfiles;
  const profiles = active.provider.startsWith("xiaomi") && typeof catalog === "function"
    ? await catalog() as ModelProfile[] : await nativeProfiles(active);
  return Promise.all(profiles.map(async profile => {
    const info = await describeProfile(profile);
    return { id: JSON.stringify([profile.provider, profile.model]), name: info.name,
      input: info.input, efforts: info.efforts, defaultEffort: profile.reasoningEffort ?? info.efforts[0] ?? "", profile };
  }));
}

/** Trusted-host continuation; never offered as a model-callable input field. */
export async function hostChat(input: { prompt: string; model: string; effort: string; snapshot?: unknown; images?: HostImage[] }) {
  if (!input || typeof input.prompt !== "string" || !input.prompt.trim()) throw new Error("host chat requires a prompt");
  const selected = (await modelCatalog()).find(model => model.id === input.model);
  if (!selected || (!selected.efforts.includes(input.effort) && !(selected.efforts.length === 0 && input.effort === ""))) throw new Error("model or effort unavailable");
  activeHost();
  const continuation: HostConversation = { snapshot: input.snapshot, images: input.images };
  try {
    await context.useModel({ ...selected.profile, reasoningEffort: input.effort });
    const result = await onHandle({ prompt: input.prompt }, continuation);
    return { result, snapshot: continuation.snapshot };
  } finally { finishHost(); }
}
