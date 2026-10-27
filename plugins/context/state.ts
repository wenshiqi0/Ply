import type { Block, Conversation, Provider } from "./types";

export const templates = new Map<string, Block>();

export const providers = new Map<string, Provider>();

export const refreshes = new Map<string, Promise<boolean>>();

export const offered = new Set<string>();

export const conversations = new Map<string, Conversation>();

/** A conversation id is the capability to use it. */
export function conversation(id: string): Conversation {
  const value = typeof id === "string" ? conversations.get(id) : undefined;
  if (!value) throw new Error("no open conversation " + id);
  return value;
}
