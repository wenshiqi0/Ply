//! Skills contribute context; their instructions are read through the existing tool.
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use ply_kernel::{
    DiskStorage, FiberEvent, FiberHook, Io, Kernel, KernelConfig, MountMode, PluginSource,
};

#[derive(Default)]
struct Records(Mutex<Vec<FiberEvent>>);
impl FiberHook for Records {
    fn on_event(&self, event: &FiberEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}
impl Records {
    fn data(&self, kind: &str) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                FiberEvent::Record { data, .. } if data["type"] == kind => Some(data.clone()),
                _ => None,
            })
            .collect()
    }
    fn reads(&self) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, FiberEvent::Spawn { fiber, .. } if fiber.plugin == "read"))
            .count()
    }
}
fn skill(root: &Path, name: &str) -> String {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let source = format!(
        "---\nname: {name}\ndescription: >-\n  Use {name} for\n  focused reviews.\nallowed-tools: create agent\n---\n# Workflow\nBODY_ONLY_SECRET\n{}\nMIDDLE_ONLY_SECRET\n{}\nEnd.\n",
        "a".repeat(12000),
        "b".repeat(12000)
    );
    std::fs::write(dir.join("SKILL.md"), source).unwrap();
    std::fs::write(dir.join("reference.txt"), "supporting resource").unwrap();
    dir.canonicalize().unwrap().display().to_string()
}
async fn kernel(
    work: &Path,
    approval: bool,
    options: Value,
    with_skills: bool,
) -> (Kernel, Arc<Records>) {
    let cwd = work.canonicalize().unwrap().display().to_string();
    let io = Io::new();
    io.mount(
        &cwd,
        Arc::new(DiskStorage::open(work).unwrap()),
        MountMode::ReadWrite,
    )
    .unwrap();
    let mut config = KernelConfig::new(io, cwd);
    if with_skills {
        config.plugin_options.insert("skill".into(), options);
    }
    if approval {
        config
            .env
            .insert("PLY_APPROVAL_MODE".into(), "ask".into());
    }
    let kernel = Kernel::new(config);
    let records = Arc::new(Records::default());
    kernel.add_hook(records.clone());
    for plugin in PluginSource::defaults() {
        let plugin = match plugin {
            PluginSource::Embedded { name, .. } if name == "skill" && !with_skills => continue,
            PluginSource::Embedded { name, source } if name == "agent" => PluginSource::Embedded {
                name,
                source: source.replacen(
                    "import { request as modelRequest } from \"ply:models\";",
                    include_str!("fixtures/skills-model.js"),
                    1,
                ),
            },
            other => other,
        };
        kernel.load(plugin).await.unwrap();
    }
    if approval {
        kernel
            .load(PluginSource::builtin("approval").unwrap())
            .await
            .unwrap();
    }
    if with_skills {
        kernel.load(PluginSource::Embedded { name: "skill_inspector".into(), source: r#"import {skill} from "ply:plugins"; export async function onHandle(input:{}){await skill.loaded;return skill.catalog;}"#.into() }).await.unwrap();
    }
    kernel.ready().await.unwrap();
    (kernel, records)
}
fn task(action: &str, expected: &[&str]) -> String {
    json!({"action":action,"expected":expected}).to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plugin_owns_options_discovery_and_context_without_becoming_a_tool() {
    let work = tempfile::tempdir().unwrap();
    skill(&work.path().join(".agents/skills"), "local-review");
    skill(&work.path().join("skills"), "second-review");
    let extra = skill(&work.path().join("extra"), "extra-review");
    skill(&work.path().join("skills/too/deep"), "nested-review");
    let broken = work.path().join("skills/broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join("SKILL.md"),
        "---\nname: Wrong\ndescription: x\n---\nbody",
    )
    .unwrap();
    let duplicate = skill(&work.path().join("override"), "local-review");
    let (kernel, _) = kernel(work.path(), false, json!({"paths":[extra,duplicate]}), true).await;
    assert!(
        !kernel
            .plugins()
            .iter()
            .find(|p| p.name == "skill")
            .unwrap()
            .callable
    );
    assert!(!kernel.catalog().iter().any(|t| t.name == "skill"));
    let agent = kernel
        .catalog()
        .into_iter()
        .find(|t| t.name == "agent")
        .unwrap();
    assert!(agent.parameters["properties"].get("skills").is_none());
    let catalog = kernel.call("skill_inspector", json!({})).await.unwrap();
    assert_eq!(catalog["skills"].as_array().unwrap().len(), 3);
    assert_eq!(catalog["skills"][0]["name"], "extra-review");
    assert_eq!(
        catalog["skills"][1]["path"],
        format!("{duplicate}/SKILL.md")
    );
    assert!(!catalog.to_string().contains("BODY_ONLY_SECRET"));
    assert!(!catalog.to_string().contains("nested-review"));
    assert!(
        catalog["warnings"]
            .to_string()
            .contains("duplicate skill name")
    );
    assert!(catalog["warnings"].to_string().contains("broken"));
    let blocks = kernel.call_ctx("blocks", vec![]).await.unwrap();
    let prompt = blocks
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "extensions")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("use read"));
    assert!(!prompt.contains("BODY_ONLY_SECRET"));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_configuration_is_isolated_and_read_activation_keeps_permissions() {
    let work = tempfile::tempdir().unwrap();
    skill(&work.path().join(".agents/skills"), "local-review");
    let alpha = skill(&work.path().join("extra-a"), "alpha-review");
    let beta = skill(&work.path().join("extra-b"), "beta-review");
    let (first, _) = kernel(work.path(), false, json!({"paths":[alpha]}), true).await;
    let (second, _) = kernel(work.path(), false, json!({"paths":[beta]}), true).await;
    let a = json!({"prompt":json!({"action":"catalog","expected":["local-review","alpha-review"],"forbidden":["beta-review"]}).to_string()});
    let b = json!({"prompt":json!({"action":"catalog","expected":["local-review","beta-review"],"forbidden":["alpha-review"]}).to_string()});
    let (a, b) = tokio::join!(first.call("agent", a), second.call("agent", b));
    assert!(
        a.unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("alpha-review")
    );
    assert!(b.unwrap()["text"].as_str().unwrap().contains("beta-review"));
    let delegated = first
        .call(
            "agent",
            json!({"prompt":task("delegate", &["local-review","alpha-review"])}),
        )
        .await
        .unwrap();
    assert_eq!(delegated["delegation"]["started"], 1);
    let (normal, records) = kernel(work.path(), false, json!({}), true).await;
    let loaded = normal.call("agent", json!({"prompt":json!({"action":"activate","expected":["local-review"],"selected":"local-review"}).to_string()})).await.unwrap();
    assert_eq!(loaded["text"], "skill fully read: local-review");
    assert!(records.reads() > 1);
    let calls = records.data("model_call");
    assert!(!calls.is_empty());
    assert!(
        calls
            .iter()
            .all(|call| call["context"]["fixed_prefix_fingerprint"]
                == calls[0]["context"]["fixed_prefix_fingerprint"])
    );
    let denied = normal.call("agent", json!({"prompt":json!({"action":"deny","expected":["local-review"],"selected":"local-review"}).to_string(),"allowed_tools":["bash"]})).await.unwrap();
    assert_eq!(denied["text"], "read grant retained");
    let escalated = normal.call("agent", json!({"prompt":json!({"action":"escalate","expected":["local-review"],"selected":"local-review"}).to_string(),"allowed_tools":["read"]})).await.unwrap();
    assert_eq!(escalated["text"], "skill cannot grant tools");
    assert!(!work.path().join("must-not-exist.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_skill_instructions_requires_the_existing_read_approval() {
    let work = tempfile::tempdir().unwrap();
    skill(&work.path().join("skills"), "approval-review");
    let (kernel, records) = kernel(work.path(), true, json!({}), true).await;
    let out = kernel.call("agent", json!({"prompt":json!({"action":"activate","expected":["approval-review"],"selected":"approval-review"}).to_string()})).await.unwrap();
    assert_eq!(out["status"], "approval_required");
    assert_eq!(records.reads(), 0);
    let requests = records.data("approval_requested");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["request"]["tool"], "read");
    assert!(!out.to_string().contains("BODY_ONLY_SECRET"));
    // A changed value must still fail binding, even though key order may differ.
    kernel.load(PluginSource::Embedded { name: "tampered_approval".into(), source: r#"
      export function onLoad() { return { async authorizeToolCall(input: any) {
        const current = await fiber.info();
        return { status: "approved", request: { schema_version: 1,
          id: current.id + ":tool-approval:tampered", tool: input.tool, cwd: input.cwd,
          input: { ...input.input, offset: 99 }, source: { fiber: current.id, parent: current.parent } } };
      } }; }
    "#.into() }).await.unwrap();
    kernel.ready().await.unwrap();
    let rejected = kernel.call("agent", json!({"prompt":json!({"action":"tamper","expected":["approval-review"],"selected":"approval-review"}).to_string()})).await.unwrap();
    assert_eq!(rejected["text"], "approval binding retained");
    assert_eq!(records.reads(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_retains_the_original_catalog_instead_of_new_plugin_options() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    unsafe {
        std::env::set_var("PLY_CHECKPOINT_DIR", state.path());
    }
    let work = tempfile::tempdir().unwrap();
    skill(&work.path().join("skills"), "original-review");
    let extra = skill(&work.path().join("extra"), "extra-review");
    let (first, _) = kernel(work.path(), false, json!({"paths":[extra]}), true).await;
    let error = first
        .call(
            "agent",
            json!({"prompt":task("pause",&["original-review","extra-review"])}),
        )
        .await
        .unwrap_err()
        .to_string();
    let resume = error
        .split("resume with agent({ resume_id: \"")
        .nth(1)
        .unwrap_or_else(|| panic!("{error}"))
        .split('"')
        .next()
        .unwrap()
        .to_string();
    drop(first);
    std::fs::remove_dir_all(work.path().join("skills/original-review")).unwrap();
    let replacement = skill(&work.path().join("skills"), "new-review");
    std::fs::write(work.path().join("continue-skills.flag"), "ready").unwrap();
    let (second, _) = kernel(work.path(), false, json!({"paths":[replacement]}), true).await;
    let out = second
        .call("agent", json!({"resume_id":resume}))
        .await
        .unwrap();
    assert_eq!(out["text"], "[\"extra-review\",\"original-review\"]");
}

#[tokio::test(flavor = "multi_thread")]
async fn skill_discovery_respects_mount_containment_and_size_budgets() {
    use std::os::unix::fs::symlink;
    let work = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let external = skill(outside.path(), "external-review");
    std::fs::create_dir_all(work.path().join("skills")).unwrap();
    symlink(&external, work.path().join("skills/external-review")).unwrap();
    let huge = skill(&work.path().join("extra"), "huge-review");
    std::fs::write(Path::new(&huge).join("SKILL.md"), "x".repeat(65537)).unwrap();
    let (world, _) = kernel(work.path(), false, json!({}), true).await;
    let catalog = world.call("skill_inspector", json!({})).await.unwrap();
    assert!(catalog["skills"].as_array().unwrap().is_empty());
    let error=world.call("read",json!({"path":work.path().canonicalize().unwrap().join("skills/external-review/SKILL.md").display().to_string()})).await.unwrap_err();
    assert!(error.to_string().contains("EACCES"));
    std::fs::create_dir_all(work.path().join("skills/huge-review")).unwrap();
    std::fs::copy(
        Path::new(&huge).join("SKILL.md"),
        work.path().join("skills/huge-review/SKILL.md"),
    )
    .unwrap();
    let (automatic, _) = kernel(work.path(), false, json!({}), true).await;
    let catalog = automatic.call("skill_inspector", json!({})).await.unwrap();
    assert!(catalog["warnings"].to_string().contains("64 KiB"));
}
