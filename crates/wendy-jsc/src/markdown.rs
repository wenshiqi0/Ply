//! Bounded CommonMark parsing. Returns data only; no HTML/URL execution or IO.
use crate::runtime::OpResult;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag};
use serde_json::{Value, json};

fn container(tag: Tag<'_>) -> Value {
    let mut node = match tag {
        Tag::Paragraph => json!({"type":"paragraph"}),
        Tag::Heading { level, .. } => json!({"type":"heading","level":level as u8}),
        Tag::BlockQuote(_) => json!({"type":"blockquote"}),
        Tag::CodeBlock(kind) => {
            json!({"type":"code_block","language":match kind {CodeBlockKind::Fenced(info)=>info.split_whitespace().next().unwrap_or("").to_string(),CodeBlockKind::Indented=>String::new()}})
        }
        Tag::HtmlBlock => json!({"type":"html_block"}),
        Tag::List(start) => json!({"type":"list","start":start}),
        Tag::Item => json!({"type":"item"}),
        Tag::Strong => json!({"type":"strong"}),
        Tag::Emphasis => json!({"type":"emphasis"}),
        Tag::Strikethrough => json!({"type":"strikethrough"}),
        Tag::Link {
            dest_url, title, ..
        } => json!({"type":"link","url":dest_url.as_ref(),"title":title.as_ref()}),
        Tag::Image {
            dest_url, title, ..
        } => json!({"type":"image","url":dest_url.as_ref(),"title":title.as_ref()}),
        Tag::Table(alignments) => {
            json!({"type":"table","alignments":alignments.iter().map(|a|match a {Alignment::Left=>"left",Alignment::Center=>"center",Alignment::Right=>"right",Alignment::None=>"none"}).collect::<Vec<_>>()})
        }
        Tag::TableHead => json!({"type":"table_head"}),
        Tag::TableRow => json!({"type":"table_row"}),
        Tag::TableCell => json!({"type":"table_cell"}),
        Tag::FootnoteDefinition(label) => json!({"type":"footnote","label":label.as_ref()}),
        _ => json!({"type":"container"}),
    };
    node["children"] = json!([]);
    node
}

pub(crate) fn parse(args: Value) -> OpResult {
    let Some(source) = args.get("source").and_then(Value::as_str) else {
        return OpResult::error("ERR_WENDY_MARKDOWN", "source must be a string");
    };
    if source.len() > 1 << 20 {
        return OpResult::error("ERR_WENDY_MARKDOWN", "markdown exceeds 1 MiB");
    }
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut stack = vec![json!({"type":"document","children":[]})];
    for (count, event) in Parser::new_ext(source, options).enumerate() {
        if count >= 100000 {
            return OpResult::error("ERR_WENDY_MARKDOWN", "markdown exceeds the node budget");
        }
        let node = match event {
            Event::Start(tag) => {
                if stack.len() >= 64 {
                    return OpResult::error("ERR_WENDY_MARKDOWN", "markdown nesting exceeds 64");
                }
                stack.push(container(tag));
                continue;
            }
            Event::End(_) => stack.pop().unwrap(),
            Event::Text(text) => json!({"type":"text","text":text.as_ref()}),
            Event::Code(text) => json!({"type":"code","text":text.as_ref()}),
            Event::Html(text) | Event::InlineHtml(text) => {
                json!({"type":"html","text":text.as_ref()})
            }
            Event::SoftBreak => json!({"type":"soft_break"}),
            Event::HardBreak => json!({"type":"hard_break"}),
            Event::Rule => json!({"type":"rule"}),
            Event::TaskListMarker(checked) => json!({"type":"task","checked":checked}),
            Event::FootnoteReference(label) => json!({"type":"footnote_ref","text":label.as_ref()}),
            _ => continue,
        };
        stack.last_mut().unwrap()["children"]
            .as_array_mut()
            .unwrap()
            .push(node);
    }
    OpResult::Json(stack.pop().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commonmark_preserves_literal_code_and_parses_structured_blocks() {
        let result = parse(
            json!({"source":"## Heading\n\n**bold** and *italic* and `**literal**` [link](https://example.com)\n\n3. ordered\n\n> quote\n\n```ts\nconst x = '**literal**';\n```\n\n- [x] done\n\n| Name | Value |\n| :--- | ---: |\n| a | 1 |\n"}),
        );
        let OpResult::Json(root) = result else {
            panic!("parse failed")
        };
        assert_eq!(root["children"][0]["type"], "heading");
        assert_eq!(root["children"][0]["level"], 2);
        let serialized = root.to_string();
        for kind in [
            "strong",
            "emphasis",
            "code",
            "link",
            "list",
            "blockquote",
            "code_block",
            "task",
            "table",
            "table_head",
            "table_cell",
        ] {
            assert!(
                serialized.contains(&format!("\"type\":\"{kind}\"")),
                "{kind}"
            );
        }
        let code = root["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["type"] == "code_block")
            .unwrap();
        assert_eq!(code["language"], "ts");
        assert!(
            code["children"][0]["text"]
                .as_str()
                .unwrap()
                .contains("**literal**")
        );
    }
    #[test]
    fn untrusted_markdown_has_size_depth_and_node_limits() {
        for source in [
            "x".repeat((1 << 20) + 1),
            format!("{}deep", "> ".repeat(80)),
            "**x** ".repeat(30000),
        ] {
            assert!(matches!(
                parse(json!({"source":source})),
                OpResult::Error(_)
            ));
        }
        let OpResult::Json(root) =
            parse(json!({"source":"\\*literal\\*\n\n<script>unsafe()</script>"}))
        else {
            panic!()
        };
        assert!(root.to_string().contains("unsafe()"));
        assert!(!root.to_string().contains("\"type\":\"emphasis\""));
    }
}
