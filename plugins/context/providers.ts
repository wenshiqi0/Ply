import plugins from "ply:plugins";
import type { Block, Conversation, Provider } from "./types";
import { MAX_BLOCK_BYTES, TYPES, blockId, changed, clone, compare, put, validateBlocks } from "./blocks";
import { conversations, providers, refreshes, templates } from "./state";

export function inheritedBlocks(parent?: Conversation): Map<string, Block> {
  const source = parent ? [...parent.blocks.values()].filter(b => b.source !== "context") : [...templates.values()];
  return validateBlocks(clone(source));
}

export async function refreshProvider(id: string, provider: Provider, query: string): Promise<boolean> {
  const name = id + ":" + blockId(provider), prior = refreshes.get(name);
  const operation = (prior?.catch(() => false) ?? Promise.resolve(false)).then(async () => {
    if (!conversations.has(id)) return false;
    let content: string | null;
    try {
      const method = plugins[provider.source][provider.method];
      if (typeof method !== "function") throw new Error("context provider method is unavailable");
      const value: unknown = await method({query:query.slice(0,20000),cwd:process.cwd()});
      if (value !== null && typeof value !== "string") throw new Error("invalid provider content");
      content = value;
      if (!(content === null || typeof content === "string") || content !== null && Buffer.byteLength(content,"utf8") > MAX_BLOCK_BYTES) throw new Error("invalid provider content");
    } catch (error) { content = "Reference provider unavailable: " + provider.source + "/" + provider.key + ". " + String(error).slice(0,1024) + ". Do not invent missing memory or treat unavailable data as deleted."; }
    const c = conversations.get(id); if (!c) return false;
    const value = changed(c.blocks, {type:provider.type,key:provider.key,content}, provider.source);
    if (!value) return false;
    put(c.blocks,value);c.pending.set(blockId(value),value);return true;
  });
  refreshes.set(name,operation);
  try { return await operation; } finally { if (refreshes.get(name) === operation) refreshes.delete(name); }
}

export async function refreshProviders(id: string, query: string) {
  for (const provider of [...providers.values()].sort((a,b)=>TYPES.indexOf(a.type)-TYPES.indexOf(b.type)||compare(a.source,b.source)||compare(a.key,b.key))) await refreshProvider(id,provider,query);
}
