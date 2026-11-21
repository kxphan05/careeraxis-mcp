//! Minimal HTML -> readable text (markdown-ish), used for job descriptions
//! and the few CareerAxis pages that have no JSON endpoint.

use scraper::{ElementRef, Html, Node, Selector};

const SKIP_TAGS: &[&str] = &[
    "script", "style", "svg", "noscript", "form", "button", "input", "select", "textarea", "iframe",
    "img", "head",
];
const SKIP_CLASSES: &[&str] = &[
    "modal",
    "social-buttons",
    "bookmark",
    "jobseeker-alerts",
    "flash-message-container",
    "fb-share-button",
];
const BLOCK_TAGS: &[&str] = &[
    "p", "div", "section", "article", "header", "footer", "ul", "ol", "table", "tr", "h1", "h2",
    "h3", "h4", "h5", "h6", "blockquote", "pre", "hr", "dl", "dt", "dd",
];

fn skip(el: &ElementRef) -> bool {
    let v = el.value();
    if SKIP_TAGS.contains(&v.name()) {
        return true;
    }
    if v.id() == Some("jobseeker-alerts") {
        return true;
    }
    v.classes().any(|c| SKIP_CLASSES.contains(&c))
}

fn walk(el: ElementRef, out: &mut String) {
    for child in el.children() {
        match child.value() {
            Node::Text(t) => {
                let s = t.replace(['\n', '\r', '\t', '\u{a0}'], " ");
                if s.trim().is_empty() {
                    if !out.ends_with([' ', '\n']) && !out.is_empty() {
                        out.push(' ');
                    }
                } else {
                    out.push_str(&s);
                }
            }
            Node::Element(_) => {
                let Some(ce) = ElementRef::wrap(child) else { continue };
                if skip(&ce) {
                    continue;
                }
                let name = ce.value().name();
                match name {
                    "br" => out.push('\n'),
                    "li" => {
                        out.push_str("\n- ");
                        walk(ce, out);
                    }
                    "a" => {
                        let before = out.len();
                        walk(ce, out);
                        let text = out[before..].trim().to_string();
                        if let Some(href) = ce.value().attr("href") {
                            if href.starts_with("http") && !text.contains(href) && !text.is_empty() {
                                out.push_str(&format!(" ({href})"));
                            }
                        }
                    }
                    "td" | "th" => {
                        walk(ce, out);
                        out.push_str(" | ");
                    }
                    _ if name.starts_with('h') && name.len() == 2 => {
                        out.push_str("\n\n## ");
                        walk(ce, out);
                        out.push_str("\n");
                    }
                    _ if BLOCK_TAGS.contains(&name) => {
                        out.push('\n');
                        walk(ce, out);
                        out.push('\n');
                    }
                    _ => walk(ce, out),
                }
            }
            _ => {}
        }
    }
}

/// Collapse runs of spaces and blank lines.
fn tidy(s: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in s.lines() {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() && lines.last().is_some_and(|p| p.is_empty()) {
            continue;
        }
        lines.push(l);
    }
    lines.join("\n").trim().to_string()
}

pub fn fragment_to_text(html: &str) -> String {
    let frag = Html::parse_fragment(html);
    let mut out = String::new();
    walk(frag.root_element(), &mut out);
    tidy(&out)
}

/// Text of the first element matching `selector` in a full page.
pub fn page_text(doc: &Html, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    let el = doc.select(&sel).next()?;
    let mut out = String::new();
    walk(el, &mut out);
    Some(tidy(&out))
}

pub fn select_text(el: ElementRef, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    let e = el.select(&sel).next()?;
    let t = e.text().collect::<Vec<_>>().join(" ");
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn converts_lists_and_links() {
        let t = fragment_to_text(
            "<h5><strong>Key</strong></h5><ul><li>One</li><li>Two <a href=\"https://x.y\">site</a></li></ul><p>End&nbsp;here</p>",
        );
        assert_eq!(t, "## Key\n\n- One\n- Two site (https://x.y)\n\nEnd here");
    }
}
