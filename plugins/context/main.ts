// What the model sees, kept in one place: the plugins offered to it as
// tools, which model to use, and each conversation. Everything lives in this
// module's closure, shared by every caller; nothing is persisted.

type Message = { role: string; content: unknown; timestamp?: number; [key: string]: unknown };
type ModelProfile = { provider: string; model: string; apiKeyEnv?: string };
type ToolDef = { name: string; description: string; parameters: unknown };

/** Longer tool output is cut to its head and tail before the model sees it. */
const MAX_TOOL_TEXT = 20_000;

const offered = new Set<string>();
let model: ModelProfile | null = null;
const conversations = new Map<string, { system: string; messages: Message[] }>();
let opened = 0;

function trimmed(message: Message): Message {
  if (message.role !== "toolResult" || !Array.isArray(message.content)) return message;
  const content = message.content.map((block: any) => {
    if (block?.type !== "text" || typeof block.text !== "string" || block.text.length <= MAX_TOOL_TEXT) return block;
    const half = MAX_TOOL_TEXT / 2;
    const omitted = block.text.length - MAX_TOOL_TEXT;
    return { ...block, text: `${block.text.slice(0, half)}\n…[${omitted} characters omitted]…\n${block.text.slice(-half)}` };
  });
  return { ...message, content };
}

function view(id: string): { systemPrompt: string; messages: Message[]; tools: ToolDef[] } {
  const conversation = conversations.get(id);
  if (!conversation) throw new Error(`no open conversation ${id}`);
  const tools_: ToolDef[] = [...offered]
    .filter((name) => tools[name]?.parameters)
    .map((name) => ({ name, description: tools[name].description, parameters: tools[name].parameters }));
  return { systemPrompt: conversation.system, messages: conversation.messages, tools: tools_ };
}

/**
 * Keeps the model's context: the tools offered to it, the model to use, and
 * the conversations in progress.
 */
export async function onLoad() {
  return {
    /** A plugin offers itself to the model as a tool. */
    registerTool(name: string) {
      offered.add(name);
    },
    /** A model integration says which model to use and where its key is. */
    useModel(profile: ModelProfile) {
      model = profile;
    },
    /** The model to use, if an integration has set one. */
    currentModel: () => model,
    /** Start a conversation; returns its id. */
    openConversation(system: string) {
      const id = `conversation-${++opened}`;
      conversations.set(id, { system, messages: [] });
      return id;
    },
    /** Add messages; returns the context the model should see now. */
    appendMessages(id: string, messages: Message[]) {
      const conversation = conversations.get(id);
      if (!conversation) throw new Error(`no open conversation ${id}`);
      conversation.messages.push(...messages.map(trimmed));
      return view(id);
    },
    closeConversation(id: string) {
      conversations.delete(id);
    },
  };
}
