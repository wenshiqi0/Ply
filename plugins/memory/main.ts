import * as commands from "ply:commands";
import * as store from "ply:memory-store";
import type { MemoryResult, MemoryToolInput, MemoryOptions, Mutation, RecallEntry, RecallPacket, Snapshot } from "./types";
import { GUIDE, configureReference, contextEntries, ranked } from "./reference";
import { context } from "ply:plugins";
import { integer, object } from "./validation";

/**
 * Explicit persistent memory. remember saves a user-requested note; recall
 * retrieves notes across conversations; forget removes an entry from memory.
 * remember/recall locate notes by key. forget accepts only action, id and
 * expected_version returned by recall; do not send key or scope to forget.
 * Never call remember/forget without an explicit user request.
 */
export async function onHandle(input: MemoryToolInput): Promise<MemoryResult> {
  const raw = object(input);
  const allowed: Record<string,string[]> = { remember:["action","scope","key","content","expected_version"],
    recall:["action","scope","key","offset","limit","content_offset","content_limit","expected_version"], forget:["action","id","expected_version"] };
  if (!Object.hasOwn(allowed,input.action)) throw new Error("invalid memory action");
  if(Object.keys(raw).some(key=>!allowed[input.action].includes(key)))throw new Error("memory "+input.action+" accepts only "+allowed[input.action].join(", "));
  if (input.scope !== undefined && !["user","project","all"].includes(input.scope)) throw new Error("invalid memory scope");
  if (input.expected_version !== undefined) integer(input.expected_version, "expected_version", 1, Number.MAX_SAFE_INTEGER);
  let result: Mutation | RecallPacket;
  if (input.action === "recall") {
    if (input.key !== undefined && (typeof input.key !== "string" || !input.key.trim())) throw new Error("invalid memory key");
    const lookupKey=input.key?.trim();
    const offset = integer(input.offset ?? 0, "offset", 0, 256), limit = integer(input.limit ?? 8, "limit", 1, 20);
    if (!input.key && (input.content_offset !== undefined || input.content_limit !== undefined)) throw new Error("content pagination requires an exact key");
    if (input.key && offset !== 0) throw new Error("use content_offset rather than entry offset for an exact key");
    const contentOffset = integer(input.content_offset ?? 0,"content_offset",0,16384), contentLimit = integer(input.content_limit ?? 4096,"content_limit",1,8000);
    const snapshot = await store.recall({ scope:input.scope ?? "all" }) as Snapshot;
    const matches = ranked(snapshot.entries.filter(entry => !lookupKey || entry.key === lookupKey), lookupKey ?? "");
    if(input.key&&matches.length>1)throw new Error("memory key exists in multiple scopes; choose user or project");
    if (input.expected_version !== undefined && matches.some(entry=>entry.version!==input.expected_version)) throw new Error("memory version changed; restart content recall");
    const entries: RecallEntry[] = [];
    const packet = () => ({project_id:snapshot.project_id,revisions:snapshot.revisions,total_matches:matches.length,entries,
      has_more:offset+entries.length<matches.length,next_offset:offset+entries.length});
    for (const entry of matches.slice(offset,offset+(input.key?1:limit))) {
      const points=[...entry.content], start=input.key?contentOffset:0;
      if(start>points.length)throw new Error("content_offset exceeds the current content");
      let length=Math.min(input.key?contentLimit:512,points.length-start);
      for (;;) {
        const end=start+length;
        entries.push({...entry,content:points.slice(start,end).join(""),content_offset:start,next_content_offset:end,
          total_content_chars:points.length,truncated:start>0||end<points.length,has_more_content:end<points.length});
        if(Buffer.byteLength(JSON.stringify(packet()),"utf8")<=16000)break;
        entries.pop();
        if(entries.length)break;
        if(length===0)throw new Error("memory entry metadata exceeds the result budget");
        length=Math.floor(length/2);
      }
      if(entries.length===0)break;
      if(entries[entries.length-1].id!==entry.id)break;
    }
    result = packet();
  } else {
    if(input.action==="forget"){
      if(typeof input.id!=="string"||!/^[a-f0-9-]{8,96}$/.test(input.id))throw new Error("forget requires an id from recall");
      integer(input.expected_version,"expected_version",1,Number.MAX_SAFE_INTEGER);
      result=await store.forget({id:input.id,expected_version:input.expected_version});
    }else{
      const { action: _action, ...note } = input;
      const args = {...note, scope: input.scope ?? "project"};
      result=await store.remember(args);
    }
  }
  return result;
}

/** Explicit file-backed memory; no automatic extraction or model calls. */
export async function onLoad(options: MemoryOptions = {}) {
  object(options);
  if (Object.keys(options).some(key => !["directory","max_context_bytes"].includes(key))) throw new Error("invalid memory plugin options");
  configureReference(integer(options.max_context_bytes ?? 8192, "max_context_bytes", 1024, 32768));
  const location = await store.configure({ directory:options.directory });
  await context.set({ source:"memory", type:context.Type.Extensions, key:"instructions", content:GUIDE });
  await context.set({ source:"memory", type:context.Type.Reference, key:"memory", content:null, provider:"contextEntries" });
  await context.registerTool("memory");
  await commands.register({ name:"memory", description:"Explicitly remember, recall, or forget persistent notes", handler:onHandle });
  return { contextEntries, location };
}
