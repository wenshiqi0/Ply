//! Binary entrypoints, including long-lived MCP framing and error responses.
use std::io::Write;
use std::process::{Command, Output, Stdio};
use serde_json::{Value, json};

fn command(work: &std::path::Path, mode: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ply"));
    command.current_dir(work).env("HOME", work.join(".test-home")).arg(mode);
    command
}
fn call(work: &std::path::Path, tool: &str, input: Value) -> Output {
    command(work, "call").arg(tool).arg(input.to_string()).output().unwrap()
}
fn value(output: &Output) -> Value {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn default_user_skills_are_visible_readable_and_read_only_including_links() {
    use std::os::unix::fs::symlink;
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let user_skills = home.path().join(".agents/skills");
    let write_skill = |root: &std::path::Path, name: &str, source: &str| {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: {source} instructions.\n---\nBODY_ONLY_{source}\n")).unwrap();
        std::fs::write(dir.join("reference.txt"), source).unwrap();
        dir.canonicalize().unwrap()
    };
    write_skill(&user_skills, "user-review", "user");
    write_skill(&user_skills, "shared-review", "user");
    let linked = write_skill(outside.path(), "linked-review", "linked");
    symlink(&linked, user_skills.join("linked-review")).unwrap();
    let project = write_skill(&work.path().join(".agents/skills"), "shared-review", "project");
    let explicit = write_skill(&work.path().join("extra"), "shared-review", "explicit");
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "not exposed by a linked skill").unwrap();
    let probe = work.path().join("probe.ts");
    std::fs::write(&probe, r#"import {skill,context,read} from "ply:plugins";
const catalog=skill.catalog;
const id=await context.openTask({prompt:"inspect skills",model:{contextWindow:131072,maxTokens:16384},allowed_tools:["read"],facts:{max_children:0,max_delegation_depth:0,report_sections:[],completion_marker:"[[DONE]]"}});
try {
const prepared=await context.prepareContext(id);
const visible=catalog.skills.every(s=>prepared.context.systemPrompt.includes(s.path));
const bodies=await Promise.all(catalog.skills.map(s=>read({path:s.path})));
console.log(JSON.stringify({catalog,visible,eagerBody:prepared.context.systemPrompt.includes("BODY_ONLY_"),bodies,cwd:process.cwd()}));
}finally{await context.closeConversation(id);}"#).unwrap();
    let run = || {
        let mut c = command(work.path(), "run");
        c.env("HOME", home.path());
        c
    };
    let report = value(&run().arg(&probe).output().unwrap());
    let skills = report["catalog"]["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 3);
    assert_eq!(report["visible"], true);
    assert_eq!(report["eagerBody"], false);
    assert_eq!(report["cwd"], work.path().canonicalize().unwrap().display().to_string());
    assert_eq!(skills.iter().find(|s| s["name"] == "shared-review").unwrap()["path"], project.join("SKILL.md").display().to_string());
    assert!(report["bodies"].to_string().contains("BODY_ONLY_linked"));
    assert!(report["catalog"]["warnings"].to_string().contains("duplicate skill name"));

    let selected = value(&run().arg("--plugin-options").arg(format!("skill={}", json!({"paths":[explicit]}))).arg(&probe).output().unwrap());
    assert_eq!(selected["catalog"]["skills"][0]["description"], "explicit instructions.");
    let mapped = value(&run().arg("--mount").arg(format!("/work=disk:{}", work.path().display())).args(["--cwd", "/work"]).arg(&probe).output().unwrap());
    assert_eq!(mapped["cwd"], "/work");
    assert_eq!(mapped["catalog"]["skills"].as_array().unwrap().len(), 3);

    for name in ["user-review", "linked-review"] {
        let output = command(work.path(), "call").env("HOME", home.path()).arg("create")
            .arg(json!({"path":user_skills.join(name).join("forbidden.txt"),"content":"bad"}).to_string()).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("EROFS"), "{}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(!user_skills.join("user-review/forbidden.txt").exists());
    assert!(!linked.join("forbidden.txt").exists());
    let denied = command(work.path(), "call").env("HOME", home.path()).arg("read")
        .arg(json!({"path":secret}).to_string()).output().unwrap();
    assert!(!denied.status.success());
}

#[test]
fn disabled_skills_do_not_mount_user_files_and_explicit_mounts_keep_their_source() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".agents/skills");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("private.txt");
    std::fs::write(&file, "user skill resource").unwrap();
    let probe = work.path().join("probe.ts");
    std::fs::write(&probe, format!(r#"import {{readFile}} from "node:fs/promises";
let visible=false;try{{await readFile({},"utf8");visible=true;}}catch{{}}
console.log(JSON.stringify({{visible,directory:process.env.PLY_USER_SKILLS_DIRECTORY??null}}));"#, json!(file))).unwrap();
    let hidden = value(&command(work.path(), "run").env("HOME", home.path())
        .arg("--no-default-plugins").arg(&probe).output().unwrap());
    assert_eq!(hidden, json!({"visible":false,"directory":null}));
    let overridden = value(&command(work.path(), "run").env("HOME", home.path())
        .args(["--mount", "disk:."]).arg("--mount").arg(format!("{}=memory", root.display()))
        .arg(&probe).output().unwrap());
    assert_eq!(overridden["visible"], false);
    assert_eq!(overridden["directory"], root.display().to_string());
}

#[test]
fn skills_disclose_context_from_plugin_options_and_use_existing_mounts() {
    let work=tempfile::tempdir().unwrap(); let external=tempfile::tempdir().unwrap();
    for (root,name) in [(work.path().join(".agents/skills"),"local-review"),(external.path().to_path_buf(),"extra-review")] {
        let dir=root.join(name); std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"),format!("---\nname: {name}\ndescription: Review {name}.\n---\nRead first.\n")).unwrap();
        std::fs::write(dir.join("reference.txt"),"resource").unwrap();
    }
    let probe=work.path().join("probe.ts");
    std::fs::write(&probe,r#"import {skill,agent} from "ply:plugins";
console.log(JSON.stringify({catalog:skill.catalog,cwd:process.cwd(),agent:agent.parameters}));"#).unwrap();
    let options=r#"skill={"paths":["/team"]}"#;
    let mounts=["--mount".to_string(),"disk:.".into(),"--mount".into(),format!("/team=disk:{}:ro",external.path().display())];
    let out=command(work.path(),"run").args(&mounts).args(["--plugin-options",options]).arg(&probe).output().unwrap();
    let report=value(&out); let skills=report["catalog"]["skills"].as_array().unwrap(); assert_eq!(skills.len(),2);
    assert_eq!(skills[0]["path"],"/team/extra-review/SKILL.md");
    assert_eq!(report["cwd"],work.path().canonicalize().unwrap().display().to_string());
    assert!(report["agent"]["properties"].get("skills").is_none());
    let read=value(&command(work.path(),"call").args(&mounts).args(["read",r#"{"path":"/team/extra-review/reference.txt"}"#]).output().unwrap()); assert_eq!(read["content"],"resource");
    let denied=command(work.path(),"call").args(&mounts).args(["create",r#"{"path":"/team/extra-review/reference.txt","content":"bad"}"#]).output().unwrap(); assert!(!denied.status.success());
    let mapped=value(&command(work.path(),"run").arg("--mount").arg(format!("/work=disk:{}",work.path().display())).args(["--cwd","/work"]).arg(&probe).output().unwrap());
    assert_eq!(mapped["catalog"]["skills"][0]["path"],"/work/.agents/skills/local-review/SKILL.md");
    let invalid=command(work.path(),"tools").args(["--plugin-options",r#"skill={"paths":"invalid"}"#]).output().unwrap(); assert!(!invalid.status.success()); assert!(String::from_utf8_lossy(&invalid.stderr).contains("skill paths"));
    let invisible=command(work.path(),"run").arg("--plugin-options").arg(format!("skill={}",json!({"paths":[external.path()]}))).arg(&probe).output().unwrap(); assert!(!invisible.status.success());
    let listed=value(&command(work.path(),"tools").output().unwrap()); let skill=listed.as_array().unwrap().iter().find(|p|p["name"]=="skill").unwrap(); assert_eq!(skill["callable"],false);
}

#[test]
fn plugin_onload_receives_only_its_own_json_options() {
    let work=tempfile::tempdir().unwrap(); let one=work.path().join("one"); let two=work.path().join("two");
    std::fs::create_dir_all(&one).unwrap(); std::fs::create_dir_all(&two).unwrap();
    std::fs::write(one.join("main.ts"),"import plugins from \"ply:plugins\"; export function onLoad(options: unknown) { return {oneOptions:options}; } export function onHandle(input: {}) { return {one:plugins.one.oneOptions,two:plugins.two.twoOptions}; }").unwrap();
    std::fs::write(two.join("main.ts"),"export function onLoad(options: unknown) { return {twoOptions:options}; }").unwrap();
    let configured=value(&command(work.path(),"call").arg("--no-default-plugins").arg("--plugin").arg(&one).arg("--plugin").arg(&two).args(["--plugin-options",r#"one={"label":"alpha"}"#,"--plugin-options",r#"two=["beta",2]"#,"one","{}"]).output().unwrap());
    assert_eq!(configured,json!({"one":{"label":"alpha"},"two":["beta",2]}));
    for args in [vec!["--plugin-options","skill=not-json"],vec!["--plugin-options","unknown={}"],vec!["--plugin-options","skill={}","--plugin-options","skill={}"]] {
        let output=command(work.path(),"tools").args(args).output().unwrap(); assert!(!output.status.success());
    }
}

#[test]
fn cli_tools_files_scripts_and_read_only_mounts_work_end_to_end() {
    let work = tempfile::tempdir().unwrap();
    let catalog = value(&command(work.path(), "tools").output().unwrap());
    for name in ["agent", "bash", "context", "create", "deepseek", "edit", "memory", "read", "remove", "skill"] {
        assert!(catalog.as_array().unwrap().iter().any(|p| p["name"] == name));
    }
    value(&call(work.path(), "create", json!({ "path": "note.txt", "content": "one\ntwo\nthree\n" })));
    let page = value(&call(work.path(), "read", json!({ "path": "note.txt", "offset": 2, "limit": 1 })));
    assert_eq!(page["content"], "two\n");
    assert_eq!(page["total_lines"], 3);
    assert_eq!(page["has_more"], true);
    value(&call(work.path(), "edit", json!({ "path": "note.txt", "old_string": "two", "new_string": "TWO" })));
    assert!(std::fs::read_to_string(work.path().join("note.txt")).unwrap().contains("TWO"));
    let shell = value(&call(work.path(), "bash", json!({ "command": "printf ply-smoke" })));
    assert_eq!(shell["stdout"], "ply-smoke");
    let denied = command(work.path(), "call").args(["--mount", "disk:.:ro", "create"])
        .arg(json!({ "path": "forbidden", "content": "x" }).to_string()).output().unwrap();
    assert!(!denied.status.success());
    assert!(!work.path().join("forbidden").exists());
    let plugin = work.path().join("echo"); std::fs::create_dir(&plugin).unwrap();
    std::fs::write(plugin.join("main.ts"), "import fs from \"node:fs\"; export function onHandle(input: string): string { return input.toUpperCase(); } export function onDispose() { fs.appendFileSync(\"disposed.txt\", \"once\"); }").unwrap();
    let script = work.path().join("script.ts");
    std::fs::write(&script, "import plugins from \"ply:plugins\";\nconst v=await plugins.echo('script'); setTimeout(()=>console.log('late timer: '+v),20);").unwrap();
    let output = command(work.path(), "run").arg("--plugin").arg(&plugin).arg(&script).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("late timer: SCRIPT"));
    // The CLI closes the kernel before exiting, so the plugin's onDispose ran once.
    assert_eq!(std::fs::read_to_string(work.path().join("disposed.txt")).unwrap(), "once");
    value(&call(work.path(), "remove", json!({ "path": "note.txt" })));
    assert!(!work.path().join("note.txt").exists());
}

#[test]
fn mcp_replies_to_bad_requests_and_keeps_handling_valid_requests() {
    let work = tempfile::tempdir().unwrap();
    let plugin = work.path().join("echo"); std::fs::create_dir(&plugin).unwrap();
    std::fs::write(plugin.join("main.ts"), "export function onHandle(input: string): string { return input.toUpperCase(); }").unwrap();
    let requests = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "echo", "arguments": { "input": "mcp" } } }),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "missing" } }),
        json!({ "jsonrpc": "2.0", "id": 5, "method": "ping" }),
        json!({ "jsonrpc": "2.0", "id": 6, "method": 42 }),
        json!({ "jsonrpc": "1.0", "id": 7, "method": "ping" }),
        json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/list", "params": 42 }),
        json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "echo", "arguments": 42 } }),
        json!({ "jsonrpc": "2.0", "id": 10 }),
        json!({ "jsonrpc": "2.0", "id": 11, "result": {} }),
        json!({ "jsonrpc": "2.0", "id": [12], "method": "ping" }),
    ];
    let mut child = command(work.path(), "mcp").arg("--plugin").arg(plugin)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in requests { writeln!(stdin, "{request}").unwrap(); }
    writeln!(stdin, "{{").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let responses: Vec<Value> = String::from_utf8(output.stdout).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let reply = |id: u64| responses.iter().find(|response| response["id"] == id).unwrap_or_else(|| panic!("missing response for {id}; received IDs: {:?}", responses.iter().map(|r| &r["id"]).collect::<Vec<_>>()));
    assert_eq!(reply(1)["result"]["serverInfo"]["name"], "ply");
    assert!(reply(2)["result"]["tools"].as_array().unwrap().iter().any(|tool| tool["name"] == "echo"));
    assert_eq!(reply(3)["result"]["content"][0]["text"], "MCP");
    assert_eq!(reply(4)["error"]["code"], -32602);
    assert_eq!(reply(5)["result"], json!({}));
    for id in [6, 7, 10] { assert_eq!(reply(id)["error"]["code"], -32600); }
    for id in [8, 9] { assert_eq!(reply(id)["error"]["code"], -32602); }
    assert!(!responses.iter().any(|response| response["id"] == 11));
    assert!(responses.iter().any(|response| response["id"].is_null() && response["error"]["code"] == -32600));
    assert!(responses.iter().any(|response| response["id"].is_null() && response["error"]["code"] == -32700));
    assert_eq!(responses.len(), 12);
}


#[test]
fn jev_is_opt_in_missing_key_requires_approval_and_invalid_configuration_is_rejected() {
    let work = tempfile::tempdir().unwrap();
    let plain = command(work.path(), "tools").env_remove("TYPESAFE_API_KEY").output().unwrap();
    assert!(plain.status.success());
    let plugins: Value = serde_json::from_slice(&plain.stdout).unwrap();
    assert!(!plugins.as_array().unwrap().iter().any(|plugin| plugin["name"] == "jev"));
    let isolated = |mode: &str| {
        let mut c = command(work.path(), mode);
        for key in ["JEV_BASE_URL", "JEV_MODEL", "JEV_TIMEOUT_MS", "JEV_MAX_STATE_BYTES", "JEV_ALLOW_MAX_RISK", "JEV_DENY_MIN_RISK"] { c.env_remove(key); }
        c
    };
    let missing = isolated("call").arg("--jev").arg("bash")
        .arg(json!({ "command": "echo unsafe > should-not-exist" }).to_string())
        .env_remove("TYPESAFE_API_KEY").output().unwrap();
    assert!(missing.status.success(), "{}", String::from_utf8_lossy(&missing.stderr));
    let pending: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(pending["status"], "approval_required");
    assert_eq!(pending["approval"]["status"], "pending");
    assert_eq!(pending["approval"]["request"]["assessments"][0]["reason_codes"][0], "missing_credentials");
    assert!(!work.path().join("should-not-exist").exists());
    let invalid = isolated("tools").arg("--jev")
        .env("TYPESAFE_API_KEY", "fixture-not-a-real-key")
        .env("JEV_ALLOW_MAX_RISK", "0.9").env("JEV_DENY_MIN_RISK", "0.1").output().unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("JEV_ALLOW_MAX_RISK"));
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("fixture-not-a-real-key"));
}

#[test]
fn cli_dispatches_registered_top_level_commands_and_lists_providers() {
    let work = tempfile::tempdir().unwrap();
    let provider = work.path().join("greeter"); std::fs::create_dir(&provider).unwrap();
    std::fs::write(provider.join("main.ts"), r#"
import * as commands from "ply:commands";
import plugins from "ply:plugins";
export async function onLoad() {
  await commands.register({name:"hello",description:"Say hello",handler:async input => ({greeting:"hello "+input.name,hidden:[typeof globalThis.fiber,typeof globalThis.ctx],legacy:typeof globalThis.tools,names:Object.keys(plugins)})});
}
"#).unwrap();
    let list = command(work.path(), "commands").args(["--no-default-plugins", "--plugin"]).arg(&provider).output().unwrap();
    assert!(list.status.success(),"{}",String::from_utf8_lossy(&list.stderr));
    let metadata: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(metadata,json!([{"name":"hello","description":"Say hello","plugin":"greeter"}]));
    let record = work.path().join("hello.jsonl");
    let output = command(work.path(), "hello").args(["--no-default-plugins", "--plugin"]).arg(&provider).arg("--record").arg(&record).arg("{\"name\":\"Ply\"}").output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output["greeting"],"hello Ply");assert_eq!(output["legacy"],"undefined");assert_eq!(output["hidden"],json!(["undefined","undefined"]));
    assert_eq!(output["names"],json!(["greeter"]));
    // The command runs as a root call; only the host journal sees the tree.
    let journal: Vec<Value> = std::fs::read_to_string(&record).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let spawn = journal.iter().find(|e| e["event"] == "spawn" && e["fiber"]["plugin"] == "greeter").unwrap();
    assert!(spawn["fiber"].get("parent").is_none());
    let bundled=command(work.path(),"commands").output().unwrap();
    assert!(bundled.status.success(),"{}",String::from_utf8_lossy(&bundled.stderr));
    let bundled: Value=serde_json::from_slice(&bundled.stdout).unwrap();
    assert!(bundled.as_array().unwrap().iter().any(|c|c["name"]=="tui"&&c["plugin"]=="tui"));
    let missing=command(work.path(),"missing-command").arg("--no-default-plugins").output().unwrap();
    assert!(!missing.status.success());assert!(String::from_utf8_lossy(&missing.stderr).contains("no registered command"));
    let help=command(work.path(),"hello").arg("--help").output().unwrap();
    assert!(help.status.success());assert!(String::from_utf8_lossy(&help.stdout).contains("Usage:"));
}
