//! Formatted text, as a message or a signature is written: a visual editor, or the HTML source
//! it can switch to.

use rune::{Block, Mark, TextEdit};

/// How formatted text is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Mode {
    /// Formatted, as it will look.
    #[default]
    Visual,
    /// As HTML source.
    Source,
}

/// The inline formatting the visual editor offers, all of which every major mail app shows.
const fn marks() -> [Mark; 7] {
    [
        Mark::Bold,
        Mark::Italic,
        Mark::Underline,
        Mark::Strike,
        Mark::Code,
        Mark::Link(String::new()),
        Mark::Color(String::new()),
    ]
}

/// The text colors offered, by name: dark enough to read on white, light enough to tell apart
/// from black.
pub(crate) const COLORS: [(&str, &str); 6] = [
    ("Gray", "#5f6368"),
    ("Red", "#d93025"),
    ("Orange", "#e37400"),
    ("Green", "#188038"),
    ("Blue", "#1a73e8"),
    ("Purple", "#9334e6"),
];

const BLOCKS: [Block; 4] = [
    Block::Bullet(0),
    Block::Numbered(0),
    Block::Quote,
    Block::Code,
];

/// A visual editor holding `html`, or plain `text` when there's no HTML.
fn visual(html: Option<&str>, text: &str) -> TextEdit {
    let edit = html.map_or_else(|| TextEdit::new(text), TextEdit::from_html);
    edit.rich().allow_marks(marks()).allow_blocks(BLOCKS)
}

/// Formatted text, edited visually or as HTML source.
#[derive(Default)]
pub(crate) struct RichEditor {
    /// The text, formatted, while in [`Mode::Visual`].
    pub(crate) text: TextEdit,
    /// The text as HTML, while in [`Mode::Source`].
    pub(crate) source: TextEdit,
    pub(crate) mode: Mode,
    /// Switching from source to visual would drop markup the editor can't show; a second press
    /// does it anyway.
    pub(crate) confirm_visual: bool,
    /// The address of the link being added to the selection, while it's typed.
    pub(crate) link: Option<TextEdit>,
    /// Whether the text colors are showing, to pick one for the selection.
    pub(crate) picking_color: bool,
}

impl RichEditor {
    /// A visual editor holding `html`, or plain `text` when there's no HTML.
    pub(crate) fn visual(html: Option<&str>, text: &str) -> Self {
        Self {
            text: visual(html, text),
            ..Self::default()
        }
    }

    /// An editor showing `html` as source.
    pub(crate) fn source(html: &str) -> Self {
        Self {
            text: visual(None, ""),
            source: TextEdit::new(html),
            mode: Mode::Source,
            ..Self::default()
        }
    }

    /// Whether what's written has any formatting, so it goes as HTML.
    pub(crate) fn is_formatted(&self) -> bool {
        match self.mode {
            Mode::Source => true,
            Mode::Visual => {
                !self.text.marks().is_empty()
                    || self.text.blocks().iter().any(|b| *b != Block::Paragraph)
            }
        }
    }

    /// What's written, as an HTML fragment (or document, if the source is one).
    pub(crate) fn html(&self) -> String {
        match self.mode {
            Mode::Visual => self.text.to_html(),
            Mode::Source => self.source.text.clone(),
        }
    }

    /// Switches between the visual editor and the HTML source. Leaving source that uses markup
    /// the editor can't show takes a second press, since that markup is dropped. Returns whether
    /// it switched.
    pub(crate) fn set_mode(&mut self, mode: Mode) -> bool {
        match mode {
            _ if mode == self.mode => {
                self.confirm_visual = false;
                false
            }
            Mode::Source => {
                self.show_source(readable(&self.text.to_html()));
                true
            }
            Mode::Visual => {
                let html = self.source.text.clone();
                let fits = postden_html::fits_editor(&html);
                self.show_visual(&html, fits)
            }
        }
    }

    /// Edits `html` as source.
    pub(crate) fn show_source(&mut self, html: String) {
        self.source = TextEdit::new(html);
        self.source.focused = true;
        self.link = None;
        self.picking_color = false;
        self.confirm_visual = false;
        self.mode = Mode::Source;
    }

    /// Edits `html` visually, unless it doesn't `fit` (it has markup the editor can't show) and
    /// this is the first try. Returns whether it switched.
    pub(crate) fn show_visual(&mut self, html: &str, fits: bool) -> bool {
        if !self.confirm_visual && !fits {
            self.confirm_visual = true;
            return false;
        }
        self.text = visual(Some(html), "");
        self.text.focused = true;
        self.confirm_visual = false;
        self.mode = Mode::Visual;
        true
    }

    /// Turns `mark` on or off for the selection, or for what's typed next.
    pub(crate) fn toggle_mark(&mut self, mark: Mark) {
        self.text.toggle_mark(mark);
        self.text.focused = true;
    }

    /// Turns the selected paragraphs into `block`s, or back into paragraphs.
    pub(crate) fn toggle_block(&mut self, block: Block) {
        self.text.toggle_block(block);
        self.text.focused = true;
    }

    /// Starts typing a link's address for the selection, or stops.
    pub(crate) fn toggle_link(&mut self) {
        if self.link.take().is_some() {
            self.text.focused = true;
            return;
        }
        let mut link = TextEdit::new(self.text.active_link().unwrap_or_default());
        link.focused = true;
        self.link = Some(link);
        self.picking_color = false;
    }

    /// Links the selection to the address typed, or unlinks it when the address is blank.
    pub(crate) fn apply_link(&mut self) {
        let Some(link) = self.link.take() else { return };
        let url = link.text.trim();
        let url = if url.is_empty() || url.contains(':') || url.starts_with('#') {
            url.to_owned()
        } else if url.contains('@') && !url.contains('/') {
            format!("mailto:{url}")
        } else {
            format!("https://{url}")
        };
        self.text.set_link(Some(&url));
        self.text.focused = true;
    }

    pub(crate) fn remove_link(&mut self) {
        self.link = None;
        self.text.set_link(None);
        self.text.focused = true;
    }

    /// Shows the text colors to pick from, or hides them.
    pub(crate) fn toggle_colors(&mut self) {
        self.picking_color = !self.picking_color;
        if self.picking_color {
            self.link = None;
        } else {
            self.text.focused = true;
        }
    }

    /// Colors the selection, or what's typed next, `color` (`#rrggbb`); `None` puts back the
    /// default color.
    pub(crate) fn set_color(&mut self, color: Option<&str>) {
        self.text.set_color(color);
        self.picking_color = false;
        self.text.focused = true;
    }

    /// Breaks the line where the caret is, as Return does in any document.
    pub(crate) fn new_line(&mut self) {
        match self.mode {
            Mode::Visual => self.text.insert("\n"),
            Mode::Source => self.source.insert("\n"),
        }
    }
}

/// The editor's HTML with a line break after each block, to read and edit as source.
pub(crate) fn readable(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + html.len() / 8);
    let mut rest = html;
    while let Some(i) = rest.find("</") {
        let close = rest.get(i..).and_then(|r| r.find('>')).map(|j| i + j + 1);
        let Some(close) = close else { break };
        let tag = &rest[i + 2..close - 1];
        out.push_str(&rest[..close]);
        if matches!(tag, "p" | "ul" | "ol" | "li" | "blockquote" | "pre" | "div") {
            out.push('\n');
        }
        rest = &rest[close..];
    }
    out.push_str(rest);
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_link_addresses_get_a_scheme() {
        let mut e = RichEditor::visual(None, "site");
        e.text.set_selection(0, 4);
        e.toggle_link();
        e.link.as_mut().unwrap().replace_text("example.com");
        e.apply_link();
        assert!(e.html().contains("https://example.com"), "{}", e.html());
    }

    #[test]
    fn simple_source_goes_back_to_visual_at_once() {
        let mut e = RichEditor::visual(None, "");
        assert!(e.set_mode(Mode::Source));
        e.source.replace_text("<p>Hi <i>you</i></p>");
        assert!(e.set_mode(Mode::Visual));
        assert!(e.text.marks().iter().any(|m| m.mark == Mark::Italic));
    }

    #[test]
    fn leaving_rich_source_takes_a_second_press() {
        let mut e = RichEditor::source("<table><tr><td>Hi</td></tr></table>");
        assert!(e.is_formatted(), "source is always HTML");
        assert!(!e.set_mode(Mode::Visual), "the first press warns");
        assert!(e.confirm_visual);
        assert!(e.set_mode(Mode::Visual), "the second goes ahead");
        assert_eq!(e.text.text.trim(), "Hi");
    }

    #[test]
    fn text_is_colored_recolored_and_reset() {
        let mut e = RichEditor::visual(None, "Hello there");
        e.text.set_selection(0, 5);
        e.toggle_colors();
        assert!(e.picking_color);
        e.set_color(Some(COLORS[1].1));
        assert!(!e.picking_color, "picking a color closes the colors");
        assert!(e.is_formatted(), "colored text goes as HTML");
        assert_eq!(e.text.active_color(), Some("#d93025"));
        assert!(
            e.html()
                .contains(r#"<span style="color:#d93025">Hello</span>"#),
            "{}",
            e.html()
        );

        e.set_color(Some(COLORS[4].1));
        assert_eq!(e.text.active_color(), Some("#1a73e8"));
        assert!(!e.html().contains("#d93025"), "{}", e.html());

        e.set_color(None);
        assert_eq!(e.text.active_color(), None);
        assert!(!e.is_formatted(), "default color is plain text");
        assert!(!e.html().contains("color"), "{}", e.html());
    }

    #[test]
    fn color_survives_the_html_source() {
        let mut e = RichEditor::visual(None, "Hello there");
        e.text.set_selection(6, 11);
        e.set_color(Some(COLORS[3].1));
        let html = e.html();
        assert!(e.set_mode(Mode::Source));
        assert!(e.source.text.contains("color:#188038"), "{}", e.source.text);
        assert!(e.set_mode(Mode::Visual), "colored source goes back at once");
        assert_eq!(e.html(), html);
        e.text.set_selection(6, 11);
        assert_eq!(e.text.active_color(), Some("#188038"));
    }

    #[test]
    fn color_source_that_fits_keeps_its_color() {
        for (style, hex) in [
            ("color:#D93025", "#d93025"),
            (" COLOR : #ABC ; ", "#aabbcc"),
            ("color: #1a73e8;", "#1a73e8"),
        ] {
            let html = format!("<p>a <span style=\"{style}\">b</span></p>");
            let mut e = RichEditor::source(&html);
            assert!(e.set_mode(Mode::Visual), "{style} fits");
            assert!(
                e.html()
                    .contains(&format!("<span style=\"color:{hex}\">b</span>")),
                "{style}: {}",
                e.html()
            );
        }
    }

    #[test]
    fn colored_code_goes_back_to_visual_at_once() {
        let mut e = RichEditor::visual(None, "Hello there");
        e.text.set_selection(0, 5);
        e.toggle_mark(Mark::Code);
        e.set_color(Some(COLORS[1].1));
        let html = e.html();
        assert!(e.set_mode(Mode::Source));
        assert!(e.set_mode(Mode::Visual), "{html}");
        assert_eq!(e.html(), html);
    }

    #[test]
    fn styles_beyond_color_still_warn() {
        let mut e = RichEditor::source(
            "<p><span style=\"color:#d93025;background-color:#ffff00\">b</span></p>",
        );
        assert!(!e.set_mode(Mode::Visual), "the first press warns");
        assert!(e.confirm_visual);
    }

    #[test]
    fn source_is_shown_a_block_per_line() {
        assert_eq!(
            readable("<p>a</p><ul><li>b</li></ul>"),
            "<p>a</p>\n<ul><li>b</li>\n</ul>"
        );
    }
}
