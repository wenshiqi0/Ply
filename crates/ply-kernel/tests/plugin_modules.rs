//! Multi-file plugins preserve module state and structural tool contracts.
use ply_kernel::{Io, Kernel, KernelConfig, PluginSource};
use serde_json::json;

fn files() -> Vec<(String, String)> {
    [
        (
            "main.ts",
            r#"
import type { Request as Input, Response as Output } from './types';
import { execute } from './nested/work';
/** Count calls in one shared module. */
export async function onHandle(input: Input): Promise<Output> { return execute(input); }
"#,
        ),
        (
            "types.ts",
            r#"
import type { Label } from './nested/label';
export interface Request {
  /** Exact text to return. */
  label: Label;
  repeat?: boolean;
}
export type Response = { text: string; calls: number };
"#,
        ),
        ("nested/label.ts", "export type Label = string;"),
        (
            "nested/work.ts",
            r#"
import type { Request, Response } from '../types';
let calls = 0;
export function execute(input: Request): Response { return { text: input.label, calls: ++calls }; }
"#,
        ),
    ]
    .into_iter()
    .map(|(path, source)| (path.into(), source.into()))
    .collect()
}

async fn assert_contract(source: PluginSource) {
    let kernel = Kernel::new(KernelConfig::new(Io::new(), "/"));
    let spec = kernel.load(source).await.unwrap();
    kernel.ready().await.unwrap();
    let schema = spec.tool_schema().unwrap();
    assert_eq!(schema.description, "Count calls in one shared module.");
    assert!(!schema.wraps_input);
    assert_eq!(schema.parameters["required"], json!(["label"]));
    assert_eq!(
        schema.parameters["properties"]["label"],
        json!({"type":"string","description":"Exact text to return."})
    );
    assert_eq!(
        spec.output.to_json_schema()["properties"]["calls"]["type"],
        "number"
    );
    assert_eq!(
        kernel
            .call(&spec.name, json!({"label":"first"}))
            .await
            .unwrap(),
        json!({"text":"first","calls":1})
    );
    assert_eq!(
        kernel
            .call(&spec.name, json!({"label":"second"}))
            .await
            .unwrap(),
        json!({"text":"second","calls":2})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bundled_modules_preserve_imported_schema_and_shared_state() {
    assert_contract(PluginSource::Bundled {
        name: "counter".into(),
        files: files(),
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn directory_modules_use_the_same_type_and_runtime_resolution() {
    let work = tempfile::tempdir().unwrap();
    let directory = work.path().join("counter");
    for (name, source) in files() {
        let path = directory.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    assert_contract(PluginSource::Dir(directory)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recursive_types_terminate_and_type_imports_cannot_escape_the_plugin_root() {
    let kernel = Kernel::new(KernelConfig::new(Io::new(), "/"));
    let source = PluginSource::Bundled { name: "recursive".into(), files: vec![
        ("main.ts".into(), "import type {Tree} from './types'; export function onHandle(input:Tree){return input;}".into()),
        ("types.ts".into(), "export interface Tree { value:string; next?:Tree }".into()),
    ] };
    let spec = kernel.load(source).await.unwrap();
    assert_eq!(
        spec.input.to_json_schema()["properties"]["value"]["type"],
        "string"
    );
    assert_eq!(spec.input.to_json_schema()["properties"]["next"], json!({}));
    let error = kernel.load(PluginSource::Bundled { name: "escape".into(), files: vec![
        ("main.ts".into(), "import type {Tree} from '../recursive/types'; export function onHandle(input:Tree){return input;}".into()),
    ] }).await.unwrap_err();
    assert!(error.to_string().contains("outside plugin root"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn bundled_modules_reject_duplicate_traversal_and_missing_entry_paths() {
    for files in [
        vec![
            ("main.ts".into(), "export function onHandle(){}".into()),
            ("../outside.ts".into(), "".into()),
        ],
        vec![("main.ts".into(), "".into()), ("main.ts".into(), "".into())],
        vec![("other.ts".into(), "export function onHandle(){}".into())],
    ] {
        let kernel = Kernel::new(KernelConfig::new(Io::new(), "/"));
        assert!(
            kernel
                .load(PluginSource::Bundled {
                    name: "invalid".into(),
                    files
                })
                .await
                .is_err()
        );
        assert!(kernel.plugins().is_empty());
    }
}
