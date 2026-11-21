//! An account's settings page: what it's called, the name its mail is sent under, its
//! signature, written in the visual editor or as HTML, with images, and the servers it reads
//! and sends mail through.

use postden_app::{NewAccount, signature};
use postden_core::account::{Account, Auth, Security, Server, Signature};
use postden_core::message::InlineImage;
use rune::{ImageCache, TextEdit};

use crate::compose::Preview;
use crate::form::{optional, port};
use crate::rich::{Mode, RichEditor, readable};

/// The file types a signature's images can be picked from.
pub(crate) const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "gif"];

/// What the page saves, to tell whether anything changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Saved {
    label: Option<String>,
    name: Option<String>,
    signature: Option<Signature>,
    servers: Option<Result<(Server, Server), String>>,
}

/// An account's IMAP and SMTP servers, and what it logs in to them with, as they're edited.
/// Blank fields fall back as they do when adding an account: the user names to the address,
/// the ports to the conventional ones, the SMTP login to the IMAP one; blank passwords keep
/// those saved.
#[derive(Default)]
pub(crate) struct Servers {
    pub(crate) username: TextEdit,
    pub(crate) imap_host: TextEdit,
    pub(crate) imap_port: TextEdit,
    pub(crate) imap_security: Security,
    pub(crate) smtp_host: TextEdit,
    pub(crate) smtp_port: TextEdit,
    pub(crate) smtp_security: Security,
    pub(crate) smtp_username: TextEdit,
    pub(crate) password: TextEdit,
    pub(crate) smtp_password: TextEdit,
}

impl Servers {
    /// The fields for `account`'s servers, user names left blank where they're the defaults.
    fn of(account: &Account) -> Self {
        let unless =
            |value: &str, default: &str| TextEdit::new(if value == default { "" } else { value });
        Self {
            username: unless(&account.imap.username, &account.id),
            imap_host: TextEdit::new(account.imap.host.clone()),
            imap_port: TextEdit::new(account.imap.port.to_string()),
            imap_security: account.imap.security,
            smtp_host: TextEdit::new(account.smtp.host.clone()),
            smtp_port: TextEdit::new(account.smtp.port.to_string()),
            smtp_security: account.smtp.security,
            smtp_username: unless(&account.smtp.username, &account.imap.username),
            password: TextEdit::new(""),
            smtp_password: TextEdit::new(""),
        }
    }

    /// The IMAP and SMTP servers as typed for the account at `address`, or what's wrong with
    /// them.
    fn build(&self, address: &str) -> Result<(Server, Server), String> {
        let account = NewAccount {
            email: address.to_owned(),
            display_name: None,
            username: optional(&self.username),
            imap_host: self.imap_host.text.clone(),
            imap_port: port(&self.imap_port, "IMAP")?,
            imap_security: self.imap_security,
            smtp_host: self.smtp_host.text.clone(),
            smtp_port: port(&self.smtp_port, "SMTP")?,
            smtp_security: self.smtp_security,
            smtp_username: optional(&self.smtp_username),
        }
        .build()
        .map_err(|e| e.to_string())?;
        Ok((account.imap, account.smtp))
    }

    /// Whether a new password was typed, for either server.
    pub(crate) const fn has_passwords(&self) -> bool {
        !self.password.text.is_empty() || !self.smtp_password.text.is_empty()
    }
}

/// An account's settings, as they're edited, while `open`.
#[derive(Default)]
pub(crate) struct AccountSettings {
    pub(crate) open: bool,
    /// The account's id.
    pub(crate) account: String,
    /// What the UI calls it, if not its name or address.
    pub(crate) label: TextEdit,
    /// The name its mail is sent under.
    pub(crate) name: TextEdit,
    /// The signature's text: in the visual editor, its images follow it; as source, they go
    /// wherever it shows them.
    pub(crate) signature: RichEditor,
    /// The signature's images, in order.
    pub(crate) images: Vec<InlineImage>,
    /// How the signature looks, as of the last edit.
    pub(crate) preview: Preview,
    /// Files are being dragged over the images.
    pub(crate) dropping: bool,
    /// Its servers and logins, as edited when [`Self::edits_servers`].
    pub(crate) servers: Servers,
    /// How it logs in.
    auth: Auth,
    /// The check of the edited servers' logins under way, by number, before they're saved.
    /// A check started from a page since closed has another number, so its answer is dropped.
    pub(crate) checking: Option<u64>,
    /// Why saving, or adding an image, failed.
    pub(crate) error: Option<String>,
    saved: Saved,
}

impl AccountSettings {
    /// The page for `account`, whose signature is `signature`. A signature written in the
    /// visual editor opens there again, unless its HTML has since become more than it shows.
    pub(crate) fn new(
        account: &Account,
        signature: Option<&Signature>,
        images: Option<&ImageCache>,
    ) -> Self {
        let editor = match signature {
            Some(s) if s.source => RichEditor::source(&s.html),
            Some(s) => match signature::to_editor(&s.html, &s.images) {
                (html, true) => RichEditor::visual(Some(&html), ""),
                (_, false) => RichEditor::source(&s.html),
            },
            None => RichEditor::visual(None, ""),
        };
        let mut page = Self {
            open: true,
            account: account.id.clone(),
            label: TextEdit::new(account.label.clone().unwrap_or_default()),
            name: TextEdit::new(account.display_name.clone().unwrap_or_default()),
            signature: editor,
            images: signature.map(|s| s.images.clone()).unwrap_or_default(),
            preview: Preview::default(),
            dropping: false,
            servers: Servers::of(account),
            auth: account.auth,
            checking: None,
            error: None,
            saved: Saved::default(),
        };
        page.saved = page.current();
        page.refresh(images);
        page
    }

    /// Whether its servers can be edited: it logs in with a password. Google's are Google's.
    pub(crate) fn edits_servers(&self) -> bool {
        self.auth == Auth::Password
    }

    /// The label to save, or `None` to show the name or address.
    pub(crate) fn label(&self) -> Option<String> {
        optional(&self.label)
    }

    /// The name to send mail under, or `None` for the address alone.
    pub(crate) fn name(&self) -> Option<String> {
        optional(&self.name)
    }

    /// The signature's HTML as it stands, its images' pills turned back into `<img>` tags.
    fn html(&self) -> String {
        match self.signature.mode {
            Mode::Visual => signature::from_editor(&self.signature.text.to_html(), &self.images),
            Mode::Source => self.signature.source.text.clone(),
        }
    }

    fn signature(&self) -> Signature {
        Signature {
            html: self.html(),
            source: self.signature.mode == Mode::Source,
            images: self.images.clone(),
        }
    }

    /// The signature as written, or `None` when it's blank.
    pub(crate) fn built(&self) -> Option<Signature> {
        Some(self.signature()).filter(|s| !signature::is_blank(s))
    }

    /// The `Content-ID`s of the images the signature shows.
    pub(crate) fn shown(&self) -> Vec<String> {
        signature::shown_images(&self.signature())
            .into_iter()
            .map(|i| i.cid)
            .collect()
    }

    fn current(&self) -> Saved {
        Saved {
            label: self.label(),
            name: self.name(),
            signature: self.built(),
            servers: self
                .edits_servers()
                .then(|| self.servers.build(&self.account)),
        }
    }

    /// `account` with the servers as edited, when they, or its passwords, changed: its logins
    /// are then checked before it's saved. `None` when they're as saved; an error when they
    /// can't be used.
    pub(crate) fn edited(&self, account: &Account) -> Result<Option<Account>, String> {
        if !self.edits_servers() {
            return Ok(None);
        }
        let built = self.servers.build(&self.account)?;
        if self.saved.servers.as_ref() == Some(&Ok(built.clone())) && !self.servers.has_passwords()
        {
            return Ok(None);
        }
        let (imap, smtp) = built;
        Ok(Some(Account {
            imap,
            smtp,
            ..account.clone()
        }))
    }

    /// Whether it's open with something that differs from what's saved.
    pub(crate) fn is_changed(&self) -> bool {
        if !self.open {
            return false;
        }
        // An editor's HTML can differ from what it was given without the text differing, so a
        // visual signature compares by what it shows, images in place.
        let shown = |s: &Option<Signature>| {
            s.as_ref().map(|s| {
                (
                    signature::text(s),
                    signature::image_widths(&s.html),
                    s.images.clone(),
                    s.source,
                )
            })
        };
        let current = self.current();
        current.label != self.saved.label
            || current.name != self.saved.name
            || current.servers != self.saved.servers
            || (self.edits_servers() && self.servers.has_passwords())
            || match (&current.signature, &self.saved.signature) {
                (Some(a), Some(b)) if a.source || b.source => a != b,
                _ => shown(&current.signature) != shown(&self.saved.signature),
            }
    }

    /// Draws the preview again, after an edit.
    pub(crate) fn refresh(&mut self, images: Option<&ImageCache>) {
        self.preview = Preview::of(&self.signature(), images);
    }

    /// Switches between the visual editor and the HTML source. Images stay where they stand:
    /// pills in the editor, `<img>` tags in the source.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        match mode {
            _ if mode == self.signature.mode => {
                self.signature.set_mode(mode);
            }
            Mode::Source => {
                let html = readable(&self.html());
                self.signature.show_source(html);
            }
            Mode::Visual => {
                let (html, fits) = signature::to_editor(&self.signature.source.text, &self.images);
                self.signature.show_visual(&html, fits);
            }
        }
    }

    /// Adds `image` to the signature where the caret is.
    pub(crate) fn add_image(&mut self, image: InlineImage) {
        let cid = image.cid.clone();
        self.images.push(image);
        self.insert_image(&cid);
        self.error = None;
    }

    /// Shows one of the signature's images where the caret is, again or elsewhere.
    pub(crate) fn insert_image(&mut self, cid: &str) {
        let Some(image) = self.images.iter().find(|i| i.cid == cid) else {
            return;
        };
        match self.signature.mode {
            Mode::Visual => {
                let (id, label) = (signature::pill_id(image), image.name.clone());
                self.signature.text.insert_pill(id, &label);
                self.signature.text.focused = true;
            }
            Mode::Source => {
                self.signature.source.insert(&signature::image_tag(image));
                self.signature.source.focused = true;
            }
        }
    }

    /// Takes an image out of the signature, wherever it shows it.
    pub(crate) fn remove_image(&mut self, cid: &str) {
        let html = signature::remove_image_tags(&self.html(), cid);
        self.images.retain(|i| i.cid != cid);
        match self.signature.mode {
            Mode::Visual => {
                let (html, _) = signature::to_editor(&html, &self.images);
                self.signature.text = RichEditor::visual(Some(&html), "").text;
            }
            Mode::Source => self.signature.source.replace_text(html),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        let server = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::None,
            username: "me".into(),
        };
        Account {
            id: "me@example.com".into(),
            display_name: Some("Ada".into()),
            label: None,
            imap: server.clone(),
            smtp: server,
            auth: Auth::Password,
        }
    }

    fn logo(cid: &str) -> InlineImage {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(64_u32.to_be_bytes());
        png.extend(32_u32.to_be_bytes());
        InlineImage {
            cid: cid.into(),
            name: format!("{cid}.png"),
            mime: "image/png".into(),
            data: png.into(),
        }
    }

    #[test]
    fn a_new_page_has_nothing_to_save() {
        assert!(!AccountSettings::default().is_changed(), "closed");
        let page = AccountSettings::new(&account(), None, None);
        assert!(!page.is_changed());
        assert_eq!(page.built(), None);
        assert_eq!(page.name().as_deref(), Some("Ada"));
        assert_eq!(page.label(), None);

        let signature = Signature {
            html: "<p><b>Ada</b> Lovelace</p>".into(),
            source: false,
            images: vec![logo("logo")],
        };
        let page = AccountSettings::new(&account(), Some(&signature), None);
        assert!(
            !page.is_changed(),
            "the editor's HTML may differ, but not what it shows"
        );
    }

    #[test]
    fn edits_are_noticed_and_blank_fields_left_out() {
        let mut page = AccountSettings::new(&account(), None, None);
        page.label.replace_text("  Work ");
        assert!(page.is_changed());
        assert_eq!(page.label().as_deref(), Some("Work"));
        page.label.replace_text(" ");
        assert!(!page.is_changed(), "blank is no label, as before");
        page.name.replace_text(" ");
        assert_eq!(page.name(), None);
        page.signature.text.replace_text("Ada");
        let built = page.built().unwrap();
        assert!(!built.source);
        assert_eq!(signature::text(&built), "Ada");
    }

    #[test]
    fn images_go_where_the_caret_is_and_stay_there() {
        let mut page = AccountSettings::new(&account(), None, None);
        page.signature.text.replace_text("John Doe\n\n1 Main St");
        page.signature.text.set_caret("John Doe\n".len());
        page.add_image(logo("logo"));
        let built = page.built().unwrap();
        assert!(!built.source);
        assert_eq!(signature::text(&built), "John Doe\n\n[logo]\n\n1 Main St");
        assert_eq!(page.shown(), ["logo"]);
        assert!(page.is_changed());

        page.set_mode(Mode::Source);
        let source = page.signature.source.text.clone();
        let at = |s: &str| source.find(s).unwrap();
        assert!(
            at("John") < at("cid:logo") && at("cid:logo") < at("Main"),
            "{source}"
        );
        assert!(source.contains("width=\"64\" height=\"32\""), "{source}");

        page.set_mode(Mode::Visual);
        assert_eq!(
            page.signature.mode,
            Mode::Visual,
            "an image is no reason to stay"
        );
        assert_eq!(page.signature.text.pills().count(), 1);
        assert_eq!(
            signature::text(&page.built().unwrap()),
            signature::text(&built)
        );

        page.add_image(logo("badge"));
        page.remove_image("logo");
        assert_eq!(page.shown(), ["badge"]);
        assert_eq!(page.images.len(), 1);
        let text = signature::text(&page.built().unwrap());
        assert!(
            text.contains("John Doe") && !text.contains("[logo]"),
            "{text}"
        );
    }

    #[test]
    fn edited_servers_are_checked_unless_as_saved() {
        let mut page = AccountSettings::new(&account(), None, None);
        assert_eq!(
            page.servers.username.text, "me",
            "not the address, so shown"
        );
        assert_eq!(page.servers.smtp_username.text, "", "the IMAP one");
        assert_eq!(page.edited(&account()), Ok(None));

        page.servers.imap_port.replace_text("");
        page.servers.imap_security = Security::Tls;
        assert!(page.is_changed());
        let edited = page.edited(&account()).unwrap().unwrap();
        assert_eq!(edited.imap.port, 993, "the conventional port");
        assert_eq!(edited.smtp, account().smtp, "untouched");
        assert_eq!(
            edited.display_name.as_deref(),
            Some("Ada"),
            "the rest is kept"
        );

        page.servers.imap_security = Security::None;
        page.servers.imap_port.replace_text("1");
        assert!(!page.is_changed(), "back as it was");
        page.servers.password.replace_text("new");
        assert!(page.is_changed());
        assert!(
            page.edited(&account()).unwrap().is_some(),
            "a new password is checked"
        );

        page.servers.smtp_host.replace_text(" ");
        assert!(page.edited(&account()).unwrap_err().contains("SMTP"));
        page.servers.smtp_host.replace_text("localhost");
        page.servers.smtp_port.replace_text("lots");
        assert!(page.edited(&account()).unwrap_err().contains("SMTP port"));
    }

    #[test]
    fn a_google_accounts_servers_are_not_edited() {
        let google = Account {
            auth: Auth::Google,
            ..account()
        };
        let mut page = AccountSettings::new(&google, None, None);
        assert!(!page.edits_servers());
        page.servers.imap_host.replace_text("elsewhere");
        page.servers.password.replace_text("new");
        assert!(!page.is_changed());
        assert_eq!(page.edited(&google), Ok(None));
    }

    #[test]
    fn a_signature_opens_as_it_was_written() {
        let visual = Signature {
            html: "<p>John Doe</p><p><img src=\"cid:logo\" alt=\"logo\" width=\"64\" height=\"32\"></p>"
                .into(),
            source: false,
            images: vec![logo("logo")],
        };
        let page = AccountSettings::new(&account(), Some(&visual), None);
        assert_eq!(page.signature.mode, Mode::Visual);
        assert_eq!(
            page.built().unwrap().html,
            visual.html,
            "saved again as it was"
        );

        let styled = Signature {
            html: "<p style=\"color:red\">John</p>".into(),
            ..visual
        };
        let page = AccountSettings::new(&account(), Some(&styled), None);
        assert_eq!(
            page.signature.mode,
            Mode::Source,
            "rather than drop the style"
        );
    }
}
