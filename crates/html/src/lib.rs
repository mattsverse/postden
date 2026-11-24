//! HTML mail: reading it as Markdown or plain text, cleaning it up to quote in a reply, and
//! scoring how well the major mail apps support a message's markup ([`compat`]).
//!
//! Nothing here fetches anything: images become their alt text and remote content is dropped.

pub mod compat;
mod dom;
mod render;
mod sanitize;

pub use sanitize::quotable;

/// `html` as `CommonMark` with GFM tables and strikethrough, to show. Styling is dropped; text is
/// escaped so nothing in it reads as Markdown. Only `http`, `https`, `mailto` and `tel` links
/// are kept.
#[must_use]
pub fn to_markdown(html: &str) -> String {
    render::markdown(&dom::parse(html))
}

/// [`to_markdown`], but images sent with the message (`src="cid:…"`) stay images,
/// `![alt](cid:…)`, for a viewer that has them, such as a preview of a signature. Other images
/// are still their alt text.
#[must_use]
pub fn to_markdown_with_cid_images(html: &str) -> String {
    render::markdown(&dom::parse_keeping_cid_images(html))
}

/// `html` as plain text: what's sent alongside it for apps that don't show HTML.
#[must_use]
pub fn to_text(html: &str) -> String {
    render::text(&dom::parse(html))
}

/// `text` with `&`, `<`, `>` and quotes escaped, to put in HTML.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Plain `text` as HTML: a paragraph per blank-line-separated block, `<br>` within one.
#[must_use]
pub fn from_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .fold(String::new(), |mut html, p| {
            html.extend([
                "<p>",
                &escape(p.trim_matches('\n')).replace('\n', "<br>"),
                "</p>",
            ]);
            html
        })
}

/// `body`, an HTML fragment, as a whole document to send. A `body` that's already a whole
/// document is sent as written.
#[must_use]
pub fn document(body: &str) -> String {
    let start = body
        .trim_start()
        .get(..15)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if start.starts_with("<!doctype") || start.starts_with("<html") {
        return body.to_owned();
    }
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n</head>\n<body>\n{body}\n</body>\n</html>\n"
    )
}

/// The elements a visual editor shows as they are: text formatting, links, lists, quotes and
/// code. Anything else in a message would be lost by editing it visually.
const EDITABLE: &[&str] = &[
    "html",
    "head",
    "body",
    "p",
    "div",
    "br",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "ins",
    "s",
    "del",
    "strike",
    "a",
    "code",
    "pre",
    "ul",
    "ol",
    "li",
    "blockquote",
    "span",
];

/// Whether `html` uses only what a visual editor keeps (see [`EDITABLE`]).
///
/// The only attributes are links' `href` and a `<span>`'s text color ([`is_text_color`])
/// outside code blocks, which the editor shows uncolored. Converting anything else to the editor drops
/// formatting.
#[must_use]
pub fn fits_editor(html: &str) -> bool {
    let is_code_block = |n: ego_tree::NodeRef<'_, scraper::Node>| {
        n.value().as_element().is_some_and(|e| e.name() == "pre")
    };
    let document = scraper::Html::parse_document(html);
    // The editor reads few character references, so a color spelled with one would be dropped.
    !styles_use_references(html)
        && document
            .tree
            .root()
            .descendants()
            .all(|node| match node.value() {
                scraper::Node::Element(e) => {
                    EDITABLE.contains(&e.name())
                        && e.attrs().all(|(name, value)| match (e.name(), name) {
                            ("a", "href") => true,
                            ("span", "style") => {
                                is_text_color(value)
                                    && !node.ancestors().any(is_code_block)
                                    && !node.descendants().any(is_code_block)
                            }
                            _ => false,
                        })
                }
                scraper::Node::Comment(_) | scraper::Node::ProcessingInstruction(_) => false,
                _ => true,
            })
}

/// Whether a `style` attribute in `html`, as written, has a character reference (`&…;`).
fn styles_use_references(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(at) = rest.find("style") {
        rest = rest.get(at + "style".len()..).unwrap_or_default();
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let value = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => value
                .get(1..)
                .and_then(|v| v.split(quote).next())
                .unwrap_or_default(),
            _ => value
                .split(|c: char| c.is_ascii_whitespace() || c == '>')
                .next()
                .unwrap_or_default(),
        };
        if value.contains('&') {
            return true;
        }
    }
    false
}

/// Whether `style` sets only the text's color, as `#rgb` or `#rrggbb`: what the visual editor
/// writes, and reads back without losing anything.
fn is_text_color(style: &str) -> bool {
    let declaration = style.trim().strip_suffix(';').unwrap_or(style).trim();
    let Some((property, value)) = declaration.split_once(':') else {
        return false;
    };
    let hex = value.trim().strip_prefix('#').unwrap_or_default();
    property.trim().eq_ignore_ascii_case("color")
        && matches!(hex.len(), 3 | 6)
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Elements that hold text without formatting it.
const PLAIN: &[&str] = &["html", "head", "body", "p", "div", "br", "span"];

/// Whether `html` is plain text: paragraphs and line breaks with no attributes, which say
/// nothing its text doesn't.
#[must_use]
pub fn is_plain(html: &str) -> bool {
    let document = scraper::Html::parse_document(html);
    document
        .tree
        .root()
        .descendants()
        .all(|node| match node.value() {
            scraper::Node::Element(e) => PLAIN.contains(&e.name()) && e.attrs().next().is_none(),
            scraper::Node::Comment(_) | scraper::Node::ProcessingInstruction(_) => false,
            _ => true,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_becomes_escaped_paragraphs() {
        assert_eq!(
            from_text("Hi <you>,\nthere\n\n\nBye & thanks"),
            "<p>Hi &lt;you&gt;,<br>there</p><p>Bye &amp; thanks</p>"
        );
    }

    #[test]
    fn fragments_are_wrapped_once() {
        let doc = document("<p>x</p>");
        assert!(doc.starts_with("<!DOCTYPE html>"), "{doc}");
        assert!(doc.contains("<body>\n<p>x</p>\n</body>"), "{doc}");
        assert_eq!(document(&doc), doc);
    }

    #[test]
    fn plain_html_is_told_apart() {
        assert!(is_plain("<p>Ada<br>Lovelace</p><p>Analyst</p>"));
        assert!(!is_plain("<p><b>Ada</b></p>"), "bold");
        assert!(!is_plain("<p style='color:red'>Ada</p>"), "style");
        assert!(!is_plain("<p><img src='cid:logo'></p>"), "image");
    }

    #[test]
    fn cid_images_can_be_kept_for_a_preview() {
        let html = "<p><b>Ada</b></p><p><img src='cid:logo@x' alt='Logo [new]'>\
                    <img src='https://track.test/p.gif' alt='Pixel'></p>";
        assert_eq!(
            to_markdown_with_cid_images(html),
            "**Ada**\n\n![Logo \\[new\\]](<cid:logo@x>)\\[Pixel\\]"
        );
        assert_eq!(
            to_markdown(html),
            "**Ada**\n\n\\[Logo \\[new\\]\\]\\[Pixel\\]",
            "the reader still shows alt text"
        );
        assert_eq!(to_text(html), "Ada\n\n[Logo [new]][Pixel]");
    }

    #[test]
    fn simple_formatting_fits_the_editor() {
        assert!(fits_editor(
            "<p><b>Hi</b> <a href='https://x.test'>x</a></p><ul><li>a</li></ul>"
        ));
        assert!(!fits_editor("<p style='color:red'>x</p>"), "style");
        assert!(!fits_editor("<p><span class='x'>x</span></p>"), "class");
        assert!(!fits_editor("<table><tr><td>x</td></tr></table>"), "table");
        assert!(!fits_editor("<!-- x --><p>x</p>"), "comment");
    }

    #[test]
    fn text_color_fits_the_editor() {
        for style in ["color:#d93025", " COLOR : #ABC ; ", "color: #1a73e8;"] {
            assert!(
                fits_editor(&format!("<p>a <span style='{style}'>b</span></p>")),
                "{style}"
            );
        }
        for style in [
            "color:red",
            "color:rgb(1,2,3)",
            "color:#d93025cc",
            "color:#d93025;font-weight:bold",
            "background-color:#d93025",
            "background:#d93025",
            "font-size:20px",
            "color:#xyz",
            "color:",
        ] {
            assert!(
                !fits_editor(&format!("<p><span style='{style}'>b</span></p>")),
                "{style}"
            );
        }
        assert!(!fits_editor("<p style='color:#d93025'>x</p>"), "only spans");
        assert!(!fits_editor("<font color='#d93025'>x</font>"), "font");
        for html in [
            "<pre><span style='color:#d93025'>x</span></pre>",
            "<pre><code><span style='color:#d93025'>x</span></code></pre>",
            "<span style='color:#d93025'><pre>x</pre></span>",
        ] {
            assert!(!fits_editor(html), "a code block shows no color: {html}");
        }
        for html in [
            "<p><span style='color:#d93025'><code>x</code></span></p>",
            "<p><code><span style='color:#d93025'>x</span></code></p>",
        ] {
            assert!(fits_editor(html), "inline code keeps its color: {html}");
        }
        assert!(
            !fits_editor("<p><span style='color:&num;d93025'>x</span></p>"),
            "a character reference"
        );
        assert!(
            !fits_editor(
                "<p><span style='color:#d93025'>a</span><span style=\"color:&#35;d93025\">b</span>\
                 <span style=color:&num;d93025>c</span></p>"
            ),
            "beside one written out"
        );
        assert!(
            fits_editor("<p>Tom &amp; Jerry <span style='color:#d93025'>x &lt; y</span></p>"),
            "a reference in text"
        );
    }
}
