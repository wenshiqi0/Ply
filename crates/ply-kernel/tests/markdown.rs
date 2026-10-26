//! CommonMark presentation through the embedded JSC module, without a model.
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;
use ply_kernel::{Io, Kernel, KernelConfig, PluginSource};

async fn renderer() -> Kernel {
    let k = Kernel::new(KernelConfig::new(Io::new(), "/"));
    k.load(PluginSource::Embedded{name:"presenter".into(),source:r#"
import * as markdown from "ply:markdown";
export function onHandle(input:{source:string;columns:number}) {return markdown.render(input.source,{columns:input.columns});}
"#.into()}).await.unwrap();
    k.ready().await.unwrap();
    k
}
fn line(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    if let Some(text) = value["text"].as_str() {
        return text.into();
    }
    value["spans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["text"].as_str().unwrap())
        .collect()
}
fn spans(value: &Value) -> Vec<&Value> {
    value
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["spans"].as_array())
        .flatten()
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn markdown_is_presented_without_delimiters_and_code_stays_literal() {
    let k = renderer().await;
    let source = "## Rendered heading\n\nA **bold** and *italic* and ~~deleted~~ with `**literal**`. [Docs](https://example.com)\n\n3. ordered\n   - nested\n\n> quoted text\n\n```ts\nconst x = '**code-literal**';\n  // keep indentation\n```\n\n- [x] done\n\n| Name | State |\n| :--- | ---: |\n| parser | ready |\n";
    let out = k
        .call("presenter", json!({"source":source,"columns":79}))
        .await
        .unwrap();
    let rendered = out
        .as_array()
        .unwrap()
        .iter()
        .map(line)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Rendered heading"));
    assert!(!rendered.contains("## Rendered heading"));
    assert!(
        !rendered.contains("**bold**")
            && !rendered.contains("*italic*")
            && !rendered.contains("~~deleted~~")
    );
    assert!(rendered.contains("**literal**") && rendered.contains("**code-literal**"));
    assert!(!rendered.contains("```"));
    assert!(rendered.contains("  // keep indentation"));
    assert!(
        rendered.contains("3. ordered")
            && rendered.contains("• nested")
            && rendered.contains("│ quoted text")
    );
    assert!(rendered.contains("☑ done"));
    assert!(rendered.contains("Docs (https://example.com)"));
    assert!(!rendered.contains("][https") && !rendered.contains("]("));
    assert!(rendered.contains("┌") && rendered.contains("parser") && !rendered.contains(":---"));
    let spans = spans(&out);
    assert!(
        spans
            .iter()
            .any(|s| s["text"] == "bold" && s["bold"] == true)
    );
    assert!(
        spans
            .iter()
            .any(|s| s["text"] == "italic" && s["italic"] == true)
    );
    assert!(
        spans
            .iter()
            .any(|s| s["text"] == "deleted" && s["strike"] == true)
    );
    assert!(
        spans
            .iter()
            .any(|s| s["text"] == "**literal**" && s["style"] == "code")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn markdown_reflows_narrow_cjk_tables_and_escapes_terminal_controls() {
    let k = renderer().await;
    let source = "# 窄窗口标题\n\n这是 **需要加粗的中文内容**，还包含 emoji 👩‍💻。\n\n| 名称 | 状态 | 说明 | 更多 |\n| --- | --- | --- | --- |\n| 解析器 | 完成 | 中文换行与内容保留 | additional details |\n\n```text\n中文缩进 **literal** 以及很长的literal-code-line-without-spaces\n```\n\nEscape \u{1b}]52;c;unsafe\u{7} and bidi \u{202e}\n";
    for columns in [79, 39, 20] {
        let out = k
            .call("presenter", json!({"source":source,"columns":columns}))
            .await
            .unwrap();
        let values: Vec<_> = out.as_array().unwrap().iter().map(line).collect();
        for value in &values {
            assert!(
                value.width() <= columns,
                "{} cells > {columns}: {value:?}",
                value.width()
            );
            assert!(!value.contains('\u{1b}') && !value.contains('\u{202e}'));
        }
        let flat = values.join("\n");
        assert!(flat.contains("解析器"));
        let code: String = spans(&out)
            .iter()
            .filter(|s| s["style"] == "code")
            .flat_map(|s| s["text"].as_str().unwrap().chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            code.contains("**literal**"),
            "literal code was changed at width {columns}"
        );
        assert!(values.join("").contains("\\u001b"));
    }
}
