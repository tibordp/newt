//! Markdown rendering for the viewer: GitHub-flavored Markdown to HTML
//! (comrak), parsed (html5ever, through dom_query) and sanitized into a
//! tree of elements and text that the page builds with the DOM API. The
//! page never parses markup, so the policy here is the whole of it: an
//! element, attribute or URL scheme that isn't listed doesn't come out.

use std::collections::HashMap;

use dom_query::{Document, NodeRef};

/// A node of a rendered document.
#[derive(Debug, PartialEq, serde::Serialize, specta::Type)]
#[serde(untagged)]
pub enum MarkdownNode {
    Text(String),
    Element {
        tag: String,
        attrs: Vec<(String, String)>,
        children: Vec<MarkdownNode>,
    },
}

/// Ids in the document carry GitHub's prefix, as GitHub's do, so `#anchor`
/// links written for GitHub resolve the same way. `ID_PREFIX` in
/// `src/viewer/markdown.ts` matches.
const ID_PREFIX: &str = "user-content-";

/// Elements nested deeper than this are flattened to their text, so a
/// pathological document cannot exhaust the stack here or in the page.
const MAX_DEPTH: usize = 128;

/// Elements kept, with the attributes each may keep besides `id` and
/// `title`. Any other element is unwrapped: its children take its place.
const ELEMENTS: &[(&str, &[&str])] = &[
    ("a", &["href"]),
    ("abbr", &[]),
    ("b", &[]),
    ("blockquote", &[]),
    ("br", &[]),
    ("caption", &[]),
    ("code", &[]),
    ("dd", &[]),
    ("del", &[]),
    ("details", &["open"]),
    ("div", &[]),
    ("dl", &[]),
    ("dt", &[]),
    ("em", &[]),
    ("h1", &[]),
    ("h2", &[]),
    ("h3", &[]),
    ("h4", &[]),
    ("h5", &[]),
    ("h6", &[]),
    ("hr", &[]),
    ("i", &[]),
    ("img", &["src", "alt", "width", "height"]),
    ("input", &[]),
    ("ins", &[]),
    ("kbd", &[]),
    ("li", &[]),
    ("mark", &[]),
    ("ol", &["start"]),
    ("p", &[]),
    ("pre", &[]),
    ("q", &[]),
    ("s", &[]),
    ("samp", &[]),
    ("section", &[]),
    ("small", &[]),
    ("span", &[]),
    ("strong", &[]),
    ("sub", &[]),
    ("summary", &[]),
    ("sup", &[]),
    ("table", &[]),
    ("tbody", &[]),
    ("td", &["align", "colspan", "rowspan"]),
    ("tfoot", &[]),
    ("th", &["align", "colspan", "rowspan"]),
    ("thead", &[]),
    ("tr", &[]),
    ("u", &[]),
    ("ul", &[]),
    ("var", &[]),
];

/// Elements dropped with everything in them: their content is code, data
/// or form state rather than text to read.
const DROPPED: &[&str] = &[
    "embed", "frame", "frameset", "head", "iframe", "noembed", "noframes", "noscript", "object",
    "option", "script", "select", "style", "template", "textarea", "title", "xmp",
];

const HTML_NS: &str = "http://www.w3.org/1999/xhtml";

/// `source` as the top-level nodes of a sanitized document. Headings get
/// GitHub-style ids for `#anchor` links.
pub fn render(source: &str) -> Vec<MarkdownNode> {
    let mut options = comrak::Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    // Raw HTML passes through to `convert`, which decides what survives.
    options.render.r#unsafe = true;
    let html = comrak::markdown_to_html(source, &options);

    // A fragment parses into a root holding an `<html>` element.
    let doc = Document::fragment(html);
    let Some(body) = doc.root().first_child() else {
        return Vec::new();
    };
    let mut slugs = Slugs::default();
    let mut nodes = Vec::new();
    for node in body.children() {
        convert(&node, 0, &mut slugs, &mut nodes);
    }
    // Whitespace between blocks renders as nothing.
    nodes.retain(|node| !matches!(node, MarkdownNode::Text(t) if t.trim().is_empty()));
    nodes
}

/// Append what `node` sanitizes to — itself, its children, or nothing — to
/// `out`.
fn convert(node: &NodeRef, depth: usize, slugs: &mut Slugs, out: &mut Vec<MarkdownNode>) {
    if node.is_text() {
        out.push(MarkdownNode::Text(node.text().to_string()));
        return;
    }
    let Some(name) = node.qual_name_ref().map(|q| q.clone()) else {
        return;
    };
    // SVG and MathML: a foreign namespace is dropped whole.
    if &*name.ns != HTML_NS {
        return;
    }
    let tag = &*name.local;
    if DROPPED.contains(&tag) {
        return;
    }
    if depth >= MAX_DEPTH {
        out.push(MarkdownNode::Text(node.text().to_string()));
        return;
    }
    let mut children = Vec::new();
    for child in node.children() {
        convert(&child, depth + 1, slugs, &mut children);
    }
    let Some(&(_, allowed)) = ELEMENTS.iter().find(|(name, _)| *name == tag) else {
        out.extend(children);
        return;
    };

    let mut attrs = Vec::new();
    for attr in node.attrs() {
        if !attr.name.ns.is_empty() {
            continue;
        }
        let (name, value) = (&*attr.name.local, attr.value.to_string());
        let keep = match name {
            "id" => {
                if !value.is_empty() {
                    attrs.push(("id".into(), format!("{ID_PREFIX}{value}")));
                }
                continue;
            }
            "title" => true,
            _ if !allowed.contains(&name) => false,
            "href" => link_allowed(&value),
            "src" => image_allowed(&value),
            _ => true,
        };
        if keep {
            attrs.push((name.to_string(), value));
        }
    }

    match tag {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            attrs.retain(|(name, _)| name != "id");
            let id = format!("{ID_PREFIX}{}", slugs.next(&text_of(&children)));
            attrs.push(("id".into(), id));
        }
        // Task list items: a checkbox that can't be changed. Any other
        // input is dropped.
        "input" => {
            if !node
                .attr("type")
                .is_some_and(|t| t.eq_ignore_ascii_case("checkbox"))
            {
                return;
            }
            attrs.retain(|(name, _)| name == "id");
            attrs.push(("type".into(), "checkbox".into()));
            attrs.push(("disabled".into(), String::new()));
            if node.has_attr("checked") {
                attrs.push(("checked".into(), String::new()));
            }
        }
        _ => {}
    }
    out.push(MarkdownNode::Element {
        tag: tag.to_string(),
        attrs,
        children,
    });
}

fn text_of(nodes: &[MarkdownNode]) -> String {
    let mut text = String::new();
    for node in nodes {
        match node {
            MarkdownNode::Text(t) => text.push_str(t),
            MarkdownNode::Element { children, .. } => text.push_str(&text_of(children)),
        }
    }
    text
}

/// A URL as `(scheme, rest)`, read more strictly than a browser reads it:
/// every control character and space is ignored, a superset of what the
/// browser strips, so `java\tscript:` is `javascript`. The scheme is
/// lowercased, and `None` for a relative URL.
fn parse_url(url: &str) -> (Option<String>, String) {
    let url: String = url.chars().filter(|&c| c > ' ').collect();
    match url.find([':', '/', '?', '#']) {
        Some(end) if url.as_bytes()[end] == b':' => (
            Some(url[..end].to_ascii_lowercase()),
            url[end + 1..].to_string(),
        ),
        _ => (None, url),
    }
}

/// Web and mail links, and relative ones (`#anchor` included); not a
/// protocol-relative `//host`, which would leave the document's origin.
fn link_allowed(url: &str) -> bool {
    match parse_url(url) {
        (Some(scheme), _) => matches!(scheme.as_str(), "http" | "https" | "mailto"),
        (None, rest) => !rest.starts_with("//"),
    }
}

/// Inline `data:image/` images and relative paths, which the page serves
/// from the document's filesystem. Nothing remote: no badges, no tracking
/// pixels.
fn image_allowed(url: &str) -> bool {
    match parse_url(url) {
        (Some(scheme), rest) => scheme == "data" && rest.to_ascii_lowercase().starts_with("image/"),
        (None, rest) => !rest.starts_with("//"),
    }
}

/// GitHub's heading anchors: lowercase, punctuation dropped, each space a
/// hyphen, and a repeat numbered.
#[derive(Default)]
struct Slugs(HashMap<String, usize>);

impl Slugs {
    fn next(&mut self, text: &str) -> String {
        let slug: String = text
            .trim()
            .to_lowercase()
            .chars()
            .filter(|&c| c.is_alphanumeric() || c.is_whitespace() || c == '_' || c == '-')
            .map(|c| if c.is_whitespace() { '-' } else { c })
            .collect();
        let seen = self.0.entry(slug.clone()).or_default();
        *seen += 1;
        match *seen {
            1 => slug,
            n => format!("{slug}-{}", n - 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(tag: &str, attrs: &[(&str, &str)], children: Vec<MarkdownNode>) -> MarkdownNode {
        MarkdownNode::Element {
            tag: tag.into(),
            attrs: attrs.iter().map(|&(n, v)| (n.into(), v.into())).collect(),
            children,
        }
    }

    fn text(s: &str) -> MarkdownNode {
        MarkdownNode::Text(s.into())
    }

    fn tags(nodes: &[MarkdownNode], out: &mut Vec<String>) {
        for node in nodes {
            if let MarkdownNode::Element { tag, children, .. } = node {
                out.push(tag.clone());
                tags(children, out);
            }
        }
    }

    fn all_tags(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        tags(&render(source), &mut out);
        out
    }

    #[test]
    fn renders_blocks_and_inlines() {
        assert_eq!(
            render("# Hi\n\nSome *em* text.\n"),
            vec![
                el("h1", &[("id", "user-content-hi")], vec![text("Hi")]),
                el(
                    "p",
                    &[],
                    vec![
                        text("Some "),
                        el("em", &[], vec![text("em")]),
                        text(" text.")
                    ]
                ),
            ]
        );
    }

    #[test]
    fn heading_ids_follow_github() {
        let ids: Vec<String> =
            render("# Getting Started\n# Getting Started\n## What's new in v2.0?\n## Über & co\n")
                .into_iter()
                .filter_map(|node| match node {
                    MarkdownNode::Element { attrs, .. } => Some(attrs[0].1.clone()),
                    MarkdownNode::Text(_) => None,
                })
                .collect();
        assert_eq!(
            ids,
            [
                "user-content-getting-started",
                "user-content-getting-started-1",
                "user-content-whats-new-in-v20",
                "user-content-über--co",
            ]
        );
    }

    #[test]
    fn drops_active_content() {
        let nodes = render(
            "<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n\
             <a href=\"javascript:alert(1)\">x</a> <iframe src=y></iframe>\n\n\
             <a href=\"java&#9;script:alert(1)\">t</a> <a href=\" JaVaScRiPt:alert(1)\">u</a>\n\n\
             <a href=\"data:text/html,alert(1)\">d</a> <img src=\"data:text/html,alert(1)\">\n\n\
             <p style=\"color:red\" class=c>s</p>\n\n<style>p{}</style>\n\n\
             <svg><script>alert(1)</script></svg> <math><mi>alert</mi></math>\n\n\
             <form action=x><input type=text><button formaction=y>b</button></form>\n\n\
             <object data=z></object> <template>alert</template>\n",
        );
        let json = serde_json::to_string(&nodes).unwrap();
        for needle in [
            "script",
            "alert",
            "onerror",
            "javascript",
            "iframe",
            "style",
            "class",
            "form",
            "button",
            "svg",
            "math",
            "object",
            "template",
            "data:",
            "\"text\"",
        ] {
            assert!(
                !json.to_lowercase().contains(needle),
                "{needle} survived: {json}"
            );
        }
    }

    #[test]
    fn unknown_elements_keep_their_text() {
        assert_eq!(
            render("<p><blink>hi</blink> <font color=red>there</font></p>\n"),
            vec![el("p", &[], vec![text("hi"), text(" "), text("there")])]
        );
    }

    #[test]
    fn urls_are_checked_by_scheme() {
        for url in [
            "https://a.b/c",
            "mailto:x@y.z",
            "#top",
            "guide.md",
            "../a b.md",
        ] {
            assert!(link_allowed(url), "{url}");
        }
        for url in [
            "javascript:x",
            "java\nscript:x",
            "file:///etc",
            "//evil/x",
            " //evil/x",
            "vbscript:x",
        ] {
            assert!(!link_allowed(url), "{url}");
        }
        for url in [
            "img/a.png",
            "data:image/png;base64,AA",
            "DATA: Image/png,AA",
        ] {
            assert!(image_allowed(url), "{url}");
        }
        for url in [
            "http://a/b.png",
            "https://a/b.png",
            "data:text/html,x",
            "data:image",
            "javascript:x",
            "//evil/x.png",
            "blob:x",
        ] {
            assert!(!image_allowed(url), "{url}");
        }
    }

    #[test]
    fn task_list_checkboxes_are_inert() {
        let nodes = render(
            "- [x] done\n- [ ] todo\n\n<input type=text value=v> <input type=CHECKBOX onclick=x>\n",
        );
        let json = serde_json::to_string(&nodes).unwrap();
        assert_eq!(
            json.matches(r#"["type","checkbox"],["disabled",""]"#)
                .count(),
            3
        );
        assert_eq!(json.matches(r#"["checked",""]"#).count(), 1);
        assert!(!json.contains(r#""text""#));
    }

    #[test]
    fn keeps_gfm_and_raw_html() {
        let tags = all_tags(
            "| a |\n|---|\n| b |\n\n~~gone~~ https://example.com\n\n\
             <details open><summary>More</summary>\n\nhidden\n\n</details>\n\n\
             note[^1]\n\n[^1]: the note\n",
        );
        for tag in ["table", "td", "del", "a", "details", "summary", "section"] {
            assert!(tags.iter().any(|t| t == tag), "{tag} missing: {tags:?}");
        }
    }

    #[test]
    fn document_ids_share_the_prefix() {
        let json = serde_json::to_string(&render(
            "<a id=\"top\"></a>\n\nnote[^1]\n\n[^1]: the note\n",
        ))
        .unwrap();
        assert!(json.contains(r#"["id","user-content-top"]"#), "{json}");
        assert!(json.contains(r##"["href","#fn-1"]"##), "{json}");
        assert!(json.contains(r#"["id","user-content-fn-1"]"#), "{json}");
    }

    fn depth(nodes: &[MarkdownNode]) -> usize {
        nodes
            .iter()
            .map(|node| match node {
                MarkdownNode::Element { children, .. } => 1 + depth(children),
                MarkdownNode::Text(_) => 0,
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn deep_nesting_flattens() {
        let nodes = render(&format!("{}deep\n", "> ".repeat(10_000)));
        assert_eq!(depth(&nodes), MAX_DEPTH);
        assert!(serde_json::to_string(&nodes).unwrap().contains("deep"));
    }
}
