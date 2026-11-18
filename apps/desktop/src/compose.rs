//! The message being written: its fields, the contacts suggested while a recipient is typed,
//! its text ([`RichEditor`]), the signature below it, the quote a reply carries, and how well
//! mail apps support its markup.

use std::collections::{BTreeSet, HashMap};

use postden_app::{Draft, Quote, contacts, signature};
use postden_core::account::Signature;
use postden_core::message::Address;
use postden_html::compat::{self, Report};
use rune::{Image, ImageCache, TextEdit};

pub(crate) use crate::rich::Mode;
use crate::rich::RichEditor;

/// The compose form's collapsible sections that are open.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Expanded {
    /// The quote of the message replied to, in full.
    pub(crate) quote: bool,
    /// What each mail app doesn't support.
    pub(crate) compat: bool,
}

/// One of the compose form's lists of recipients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recipients {
    To,
    Cc,
    Bcc,
}

impl Recipients {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::To => "To",
            Self::Cc => "Cc",
            Self::Bcc => "Bcc",
        }
    }

    /// The field's key, which its suggestions are anchored to.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::To => "compose-to",
            Self::Cc => "compose-cc",
            Self::Bcc => "compose-bcc",
        }
    }

    /// The key of its list of suggestions.
    pub(crate) fn suggestions_key(self) -> String {
        format!("{}-suggestions", self.key())
    }

    /// The key of its `i`th suggestion's row, as the list names it.
    pub(crate) fn suggestion_key(self, i: usize) -> String {
        format!("{}-option-{i}", self.suggestions_key())
    }
}

/// The contacts suggested for the recipient being typed in one of the lists.
#[derive(Debug, Default)]
pub(crate) struct Suggestions {
    field: Option<Recipients>,
    /// The field's text and caret they were found for: edited or moved since, they're stale.
    typed: (String, usize),
    contacts: Vec<Address>,
    /// The one Return or Tab picks.
    pub(crate) active: usize,
}

/// A signature as it's shown: as Markdown, with its images.
#[derive(Default)]
pub(crate) struct Preview {
    pub(crate) markdown: String,
    /// The pictures of its images, by `cid:` source.
    pub(crate) pictures: HashMap<String, Image>,
    /// The width each image is sent at, by `cid:` source, when the signature sets it.
    pub(crate) widths: HashMap<String, f32>,
}

impl Preview {
    /// How `signature` looks, its pictures decoded by `images`, if there's a cache.
    pub(crate) fn of(signature: &Signature, images: Option<&ImageCache>) -> Self {
        let source = |cid: &str| format!("cid:{cid}");
        Self {
            markdown: postden_html::to_markdown_with_cid_images(&signature.html),
            pictures: images.map_or_else(HashMap::new, |cache| {
                signature
                    .images
                    .iter()
                    .map(|i| (source(&i.cid), cache.load(std::sync::Arc::clone(&i.data))))
                    .collect()
            }),
            widths: signature::image_widths(&signature.html)
                .into_iter()
                .filter_map(|(cid, w)| Some((source(&cid), f32::from(u16::try_from(w).ok()?))))
                .collect(),
        }
    }
}

/// A message being written, while `open`.
#[derive(Default)]
pub(crate) struct ComposeForm {
    pub(crate) open: bool,
    /// The account it's sent from.
    pub(crate) account: String,
    pub(crate) to: TextEdit,
    pub(crate) cc: TextEdit,
    pub(crate) bcc: TextEdit,
    /// The contacts suggested for the recipient being typed.
    pub(crate) suggestions: Suggestions,
    pub(crate) subject: TextEdit,
    /// What's written.
    pub(crate) body: RichEditor,
    /// The account's signature, below what's written.
    pub(crate) signature: Option<Signature>,
    /// How the signature looks.
    pub(crate) signature_preview: Preview,
    /// The features the signature's markup uses, found once.
    signature_features: BTreeSet<String>,
    /// The message replied to, or forwarded, quoted below.
    pub(crate) quote: Option<Quote>,
    /// The features the quote's markup uses, found once.
    quote_features: BTreeSet<String>,
    /// The quote's HTML as Markdown, to show, converted once.
    pub(crate) quote_markdown: Option<String>,

    /// How well mail apps support the message's markup, as of the last edit.
    pub(crate) compat: Report,
    /// Which of the form's collapsible sections are open.
    pub(crate) expanded: Expanded,
    in_reply_to: Option<String>,
    references: Vec<String>,
    /// It passes on the message it quotes.
    forward: bool,
    /// The mailroom is sending it.
    pub(crate) sending: bool,
    /// Why it couldn't be sent.
    pub(crate) error: Option<String>,
}

impl ComposeForm {
    /// The form for `draft`, focused where typing starts: the recipients of a new message, or the
    /// top of a reply's text, above the quote. The signature's pictures are decoded by `images`.
    pub(crate) fn new(draft: Draft, images: Option<&ImageCache>) -> Self {
        let quote_features = draft
            .quote
            .as_ref()
            .map(|q| compat::features(&q.as_html()))
            .unwrap_or_default();
        let mut form = Self {
            open: true,
            account: draft.account,
            to: TextEdit::new(draft.to),
            cc: TextEdit::new(draft.cc),
            bcc: TextEdit::new(draft.bcc),
            suggestions: Suggestions::default(),
            subject: TextEdit::new(draft.subject),
            body: RichEditor::visual(draft.html.as_deref(), &draft.text),
            signature: None,
            signature_preview: Preview::default(),
            signature_features: BTreeSet::new(),
            quote_markdown: draft
                .quote
                .as_ref()
                .and_then(|q| q.html.as_deref())
                .map(postden_html::to_markdown),
            quote: draft.quote,
            quote_features,
            compat: Report::default(),
            expanded: Expanded::default(),
            in_reply_to: draft.in_reply_to,
            references: draft.references,
            forward: draft.forward,
            sending: false,
            error: None,
        };
        if form.in_reply_to.is_some() {
            form.body.text.set_caret(0);
            form.body.text.focused = true;
        } else {
            form.to.focused = true;
        }
        form.set_signature(draft.signature, images);
        form
    }

    /// Whether it's open and being sent.
    pub(crate) const fn is_sending(&self) -> bool {
        self.open && self.sending
    }

    pub(crate) const fn is_reply(&self) -> bool {
        self.in_reply_to.is_some()
    }

    /// Whether it passes on a message.
    pub(crate) const fn is_forward(&self) -> bool {
        self.forward
    }

    pub(crate) const fn recipients(&self, field: Recipients) -> &TextEdit {
        match field {
            Recipients::To => &self.to,
            Recipients::Cc => &self.cc,
            Recipients::Bcc => &self.bcc,
        }
    }

    pub(crate) const fn recipients_mut(&mut self, field: Recipients) -> &mut TextEdit {
        match field {
            Recipients::To => &mut self.to,
            Recipients::Cc => &mut self.cc,
            Recipients::Bcc => &mut self.bcc,
        }
    }

    /// The contacts to suggest under `field`: some were found for what it holds, and it's still
    /// being typed in.
    pub(crate) fn suggested(&self, field: Recipients) -> &[Address] {
        let s = &self.suggestions;
        let edit = self.recipients(field);
        let current = s.field == Some(field)
            && edit.focused
            && !edit.is_composing()
            && !self.sending
            && s.typed.0 == edit.text
            && s.typed.1 == edit.focus;
        if current { &s.contacts } else { &[] }
    }

    /// Suggests `contacts` for what `field` holds now, the first picked.
    pub(crate) fn suggest(&mut self, field: Recipients, contacts: Vec<Address>) {
        let edit = self.recipients(field);
        self.suggestions = Suggestions {
            field: Some(field),
            typed: (edit.text.clone(), edit.focus),
            contacts,
            active: 0,
        };
    }

    /// Stops suggesting contacts, until something else is typed.
    pub(crate) fn close_suggestions(&mut self) {
        self.suggestions = Suggestions::default();
    }

    /// Picks the suggestion `by` rows down, or up when negative, wrapping around.
    pub(crate) fn move_suggestion(&mut self, field: Recipients, by: isize) {
        let count = self.suggested(field).len();
        if count == 0 {
            return;
        }
        let active = self.suggestions.active.min(count - 1);
        self.suggestions.active = if by < 0 {
            (active + count - by.unsigned_abs() % count) % count
        } else {
            (active + by.unsigned_abs()) % count
        };
    }

    /// Puts the `i`th suggestion in place of the recipient being typed in `field`, followed by a
    /// comma to type the next one, and stops suggesting. Whether there was one to put.
    pub(crate) fn choose_suggestion(&mut self, field: Recipients, i: usize) -> bool {
        let Some(contact) = self.suggested(field).get(i).cloned() else {
            return false;
        };
        let edit = self.recipients_mut(field);
        let (range, written) = contacts::complete(&edit.text, edit.focus, &contact);
        // One edit, which undo takes back, as it does what was typed before it.
        edit.set_selection(range.start, range.end);
        edit.insert(&written);
        edit.focused = true;
        self.close_suggestions();
        true
    }

    /// Scores the markup again, after an edit.
    pub(crate) fn refresh(&mut self) {
        let mut used = if self.body.is_formatted() {
            compat::features(&self.body.html())
        } else {
            BTreeSet::new()
        };
        used.extend(self.signature_features.iter().cloned());
        used.extend(self.quote_features.iter().cloned());
        self.compat = compat::report(&used);
    }

    /// The fields as a draft. Unformatted text goes as plain text.
    pub(crate) fn draft(&self) -> Draft {
        let (text, html) = if self.body.is_formatted() {
            let html = self.body.html();
            (postden_html::to_text(&html), Some(html))
        } else {
            (self.body.text.text.clone(), None)
        };
        Draft {
            account: self.account.clone(),
            to: self.to.text.clone(),
            cc: self.cc.text.clone(),
            bcc: self.bcc.text.clone(),
            subject: self.subject.text.clone(),
            text,
            html,
            signature: self.signature.clone(),
            quote: self.quote.clone(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            forward: self.forward,
        }
    }

    /// Puts `signature` below what's written, in place of any other, as when the message is sent
    /// from another account.
    pub(crate) fn set_signature(
        &mut self,
        signature: Option<Signature>,
        images: Option<&ImageCache>,
    ) {
        self.signature_preview = signature
            .as_ref()
            .map(|s| Preview::of(s, images))
            .unwrap_or_default();
        self.signature_features = signature
            .as_ref()
            .filter(|s| signature::is_formatted(s))
            .map(|s| compat::features(&s.html))
            .unwrap_or_default();
        self.signature = signature;
        self.refresh();
    }

    /// Switches between the visual editor and the HTML source.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        if self.body.set_mode(mode) {
            self.refresh();
        }
    }

    /// Drops the quote of the message replied to.
    pub(crate) fn remove_quote(&mut self) {
        self.quote = None;
        self.quote_markdown = None;
        self.quote_features.clear();
        self.refresh();
    }
}

#[cfg(test)]
mod tests {
    use postden_app::Quote;

    use super::*;

    #[test]
    fn a_compose_form_gives_back_its_draft() {
        let draft = Draft {
            to: "ada@example.com".into(),
            subject: "Re: Lunch".into(),
            text: "Noon works.".into(),
            signature: Some(Signature {
                html: "<p>Me</p>".into(),
                source: false,
                images: Vec::new(),
            }),
            quote: Some(Quote {
                header: "Ada wrote:".into(),
                text: "Noon?".into(),
                html: None,
            }),
            in_reply_to: Some("parent@example.com".into()),
            references: vec!["parent@example.com".into()],
            ..Draft::new("me@example.com")
        };
        let form = ComposeForm::new(draft.clone(), None);
        assert!(form.is_reply());
        assert!(form.body.text.focused, "a reply starts in its text");
        assert_eq!(form.draft(), draft, "plain text stays plain");
        assert_eq!(form.signature_preview.markdown, "Me");
        assert!(
            ComposeForm::new(Draft::new("me@example.com"), None)
                .to
                .focused,
            "a new message starts at its recipients"
        );
    }

    fn contact(name: &str, email: &str) -> Address {
        Address {
            name: Some(name.into()),
            email: Some(email.into()),
        }
    }

    /// A form being typed in To, `text` with the caret at its end.
    fn typing_to(text: &str) -> ComposeForm {
        let mut form = ComposeForm::new(Draft::new("me@example.com"), None);
        form.to.replace_text(text);
        form.to.set_caret(text.len());
        form
    }

    #[test]
    fn a_suggestion_picked_takes_the_place_of_what_was_typed() {
        let mut form = typing_to("bob@example.com, ad");
        let found = vec![
            contact("Ada", "ada@example.com"),
            contact("Adam", "adam@example.org"),
        ];
        form.suggest(Recipients::To, found.clone());
        assert_eq!(form.suggested(Recipients::To), found.as_slice());
        assert!(form.suggested(Recipients::Cc).is_empty(), "only under To");

        form.move_suggestion(Recipients::To, -1);
        assert_eq!(
            form.suggestions.active, 1,
            "up from the first wraps to the last"
        );
        form.move_suggestion(Recipients::To, 1);
        assert_eq!(form.suggestions.active, 0);
        assert!(form.choose_suggestion(Recipients::To, 1));
        assert_eq!(form.to.text, "bob@example.com, Adam <adam@example.org>, ");
        assert_eq!(form.to.focus, form.to.text.len(), "the caret is after it");
        assert!(
            form.suggested(Recipients::To).is_empty(),
            "picked, they're gone"
        );
        assert!(!form.choose_suggestion(Recipients::To, 0));
        form.to.undo();
        assert_eq!(form.to.text, "bob@example.com, ad", "undo takes it back");
    }

    #[test]
    fn suggestions_for_what_was_typed_before_are_stale() {
        let mut form = typing_to("ad");
        form.suggest(Recipients::To, vec![contact("Ada", "ada@example.com")]);
        form.to.replace_text("adx");
        form.to.set_caret(3);
        assert!(form.suggested(Recipients::To).is_empty(), "edited since");

        let mut form = typing_to("ad");
        form.suggest(Recipients::To, vec![contact("Ada", "ada@example.com")]);
        form.to.focused = false;
        assert!(
            form.suggested(Recipients::To).is_empty(),
            "typing moved elsewhere"
        );
        form.to.focused = true;
        assert_eq!(form.suggested(Recipients::To).len(), 1, "and came back");
        form.sending = true;
        assert!(
            form.suggested(Recipients::To).is_empty(),
            "nor while sending"
        );
    }

    #[test]
    fn formatting_is_sent_as_html_with_a_text_version() {
        let mut form = ComposeForm::new(Draft::new("me@example.com"), None);
        form.body.text.replace_text("Hello there");
        form.body.text.set_selection(6, 11);
        form.body.toggle_mark(rune::Mark::Bold);
        form.refresh();
        let draft = form.draft();
        assert_eq!(draft.text, "Hello there");
        let html = draft.html.unwrap();
        assert!(
            html.contains("<b>there</b>") || html.contains("<strong>there</strong>"),
            "{html}"
        );
        assert_eq!(form.compat.percent(), 100, "{:?}", form.compat);
    }

    #[test]
    fn source_mode_scores_its_markup() {
        let mut form = ComposeForm::new(Draft::new("me@example.com"), None);
        form.body.text.replace_text("Hi");
        form.set_mode(Mode::Source);
        assert_eq!(form.body.mode, Mode::Source);
        #[expect(
            clippy::literal_string_with_formatting_args,
            reason = "CSS, not a format string"
        )]
        form.body
            .source
            .replace_text("<style>p{display:flex}</style><p style='display:grid'>Hi</p>");
        form.refresh();
        assert!(form.compat.percent() < 100, "{:?}", form.compat);
        assert!(form.draft().html.unwrap().contains("display:grid"));

        form.set_mode(Mode::Visual);
        assert_eq!(form.body.mode, Mode::Source, "the first press warns");
        form.set_mode(Mode::Visual);
        assert_eq!(form.body.mode, Mode::Visual, "the second goes ahead");
        assert_eq!(form.body.text.text.trim(), "Hi");
    }

    #[test]
    fn the_quote_and_signature_count_toward_the_score() {
        let mut draft = Draft::new("me@example.com");
        draft.quote = Some(Quote {
            header: "Ada wrote:".into(),
            text: "x".into(),
            html: Some("<div style=\"display:flex\">x</div>".into()),
        });
        let mut form = ComposeForm::new(draft, None);
        assert!(form.compat.percent() < 100, "{:?}", form.compat);
        form.remove_quote();
        assert_eq!(form.compat.percent(), 100);

        let flex = Signature {
            html: "<div style=\"display:flex\">Me</div>".into(),
            source: true,
            images: Vec::new(),
        };
        form.set_signature(Some(flex), None);
        assert!(form.compat.percent() < 100, "{:?}", form.compat);
        form.set_signature(None, None);
        assert_eq!(form.compat.percent(), 100);
        assert_eq!(form.draft().signature, None, "removed");
    }
}
