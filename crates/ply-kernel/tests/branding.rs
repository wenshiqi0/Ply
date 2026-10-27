//! The public module namespace is ply; retired names are not runtime aliases.
use ply_kernel::{Io, Kernel, KernelConfig, PluginSource};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn ply_named_exports_work_and_legacy_module_and_environment_are_not_aliased() {
    let mut config = KernelConfig::new(Io::new(), "/");
    // Invalid legacy configuration must not be read by the new namespace.
    config
        .env
        .insert("WENDY_CONTEXT_CONFIG".into(), "not-json".into());
    let kernel = Kernel::new(config);
    kernel
        .load(PluginSource::builtin("context").unwrap())
        .await
        .unwrap();
    kernel
        .load(PluginSource::Embedded {
            name: "brand_probe".into(),
            source: r#"
          import plugins, {context} from "ply:plugins";
          export async function onHandle(input: {}) {
            let rejected = false;
            try { await import("wendy:plugins"); } catch { rejected = true; }
            return { same: context === plugins.context, rejected, type: context.Type.Extensions };
          }
        "#
            .into(),
        })
        .await
        .unwrap();
    kernel.ready().await.unwrap();
    assert_eq!(
        kernel.call("brand_probe", json!({})).await.unwrap(),
        json!({
            "same": true, "rejected": true, "type": "extensions"
        })
    );
}
