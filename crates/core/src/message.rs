//! What Postden knows about a message without downloading its body, and what it sends.

use std::fmt;
use std::sync::Arc;

/// A sender or recipient.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Address {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl Address {
    /// The name to show: the display name, or else the address, or else an empty string.
    #[must_use]
    pub fn label(&self) -> &str {
        self.name
            .as_deref()
            .filter(|n| !n.is_empty())
            .or(self.email.as_deref())
            .unwrap_or_default()
    }

    /// The name and address, as in a header: `Ada <ada@example.com>`. Just one of them if the other
    /// is missing.
    #[must_use]
    pub fn full(&self) -> String {
        match (self.name.as_deref().filter(|n| !n.is_empty()), &self.email) {
            (Some(name), Some(email)) => format!("{name} <{email}>"),
            _ => self.label().to_owned(),
        }
    }
}

/// A message's headers and flags, enough to list it in a mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Envelope {
    /// The message's IMAP UID. It's unique within its mailbox while the mailbox's UIDVALIDITY holds.
    pub uid: u32,
    /// The `Message-ID` header, without angle brackets.
    pub message_id: Option<String>,
    pub from: Address,
    /// Who it was sent to, from its `To` header.
    pub to: Vec<Address>,
    pub subject: Option<String>,
    /// When the message was sent, as Unix seconds, from its `Date` header.
    pub date: Option<i64>,
    /// Whether the message has been read.
    pub seen: bool,
    /// Whether the message is starred: flagged `\\Flagged` on the server.
    pub flagged: bool,
}

/// What a mailbox is for. Each account has one inbox and at most one Sent, Archive, Trash,
/// Snoozed and Junk mailbox, though their names differ by server; every other mailbox is
/// [`Role::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Role {
    /// Where mail arrives: `INBOX`.
    #[default]
    Inbox,
    /// Where sent mail is kept, such as `Sent` or `[Gmail]/Sent Mail`.
    Sent,
    /// Where messages go when they're archived, such as `Archive` or Gmail's `[Gmail]/All Mail`.
    Archive,
    /// Where messages go when they're deleted, such as `Trash` or `Deleted Items`.
    Trash,
    /// Where snoozed messages wait until they're due back in the inbox, such as `Snoozed`.
    Snoozed,
    /// Where spam goes, such as `Junk` or Gmail's `[Gmail]/Spam`.
    Junk,
    /// Where drafts are kept, such as `Drafts` or Gmail's `[Gmail]/Drafts`.
    Drafts,
    /// Any other mailbox: folders.
    Other,
}

impl Role {
    /// A stable name for this role, safe to persist.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Sent => "sent",
            Self::Archive => "archive",
            Self::Trash => "trash",
            Self::Snoozed => "snoozed",
            Self::Junk => "junk",
            Self::Drafts => "drafts",
            Self::Other => "other",
        }
    }

    /// The role named by [`Role::as_str`], if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Inbox,
            Self::Sent,
            Self::Archive,
            Self::Trash,
            Self::Snoozed,
            Self::Junk,
            Self::Drafts,
            Self::Other,
        ]
        .into_iter()
        .find(|r| r.as_str() == name)
    }
}

/// A mailbox on the server that messages can be listed from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Mailbox {
    /// Its full name, such as `INBOX`, `Archive/2026` or `[Gmail]/Sent Mail`.
    pub name: String,
    /// What separates levels in `name`, such as `/` or `.`, if the server nests mailboxes.
    pub delimiter: Option<String>,
    pub role: Role,
}

impl Mailbox {
    /// The name to show: read from modified UTF-7 if it's spelled so, its levels joined with
    /// " / ", and the inbox as "Inbox".
    #[must_use]
    pub fn label(&self) -> String {
        if self.role == Role::Inbox {
            return "Inbox".to_owned();
        }
        let name = crate::mutf7::decode(&self.name).unwrap_or_else(|| self.name.clone());
        self.delimiter
            .as_deref()
            .filter(|d| !d.is_empty())
            .map_or_else(
                || name.clone(),
                |d| name.split(d).collect::<Vec<_>>().join(" / "),
            )
    }
}

/// How far one mailbox has been synced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MailboxState {
    /// The server's UIDVALIDITY when the cached messages were fetched. If it changes, they're stale.
    pub uid_validity: u32,
    /// The highest UID cached, or `None` when nothing is cached yet.
    pub last_uid: Option<u32>,
}

/// What opening a message downloads on top of its [`Envelope`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Body {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Where replies should go instead of the sender, from the `Reply-To` header.
    pub reply_to: Vec<Address>,
    /// The thread this message belongs to: its `References` header, oldest first, without angle
    /// brackets.
    pub references: Vec<String>,
    /// The readable text: the plain-text part, or the HTML part converted to text.
    pub text: String,
    /// The HTML part as received, if there is one. It may load remote content and run scripts
    /// in a browser, so it's for converting, never for showing as is.
    pub html: Option<String>,
    /// The attachments' file names, or a placeholder for those without one.
    pub attachments: Vec<String>,
}

/// An image sent inside a message rather than fetched: its HTML shows it with `src="cid:…"`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct InlineImage {
    /// Its `Content-ID`, without angle brackets, as the HTML refers to it after `cid:`.
    pub cid: String,
    /// The file name it was added from, such as `logo.png`.
    pub name: String,
    /// Its MIME type, such as `image/png`.
    pub mime: String,
    pub data: Arc<[u8]>,
}

impl fmt::Debug for InlineImage {
    /// Everything but the bytes, which only their length stands for.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InlineImage")
            .field("cid", &self.cid)
            .field("name", &self.name)
            .field("mime", &self.mime)
            .field("bytes", &self.data.len())
            .finish()
    }
}

/// A message ready to send: plain text, and optionally HTML too.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Outgoing {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Recipients left out of the headers.
    pub bcc: Vec<Address>,
    pub subject: String,
    /// The plain-text version, for apps that don't show HTML.
    pub text: String,
    /// A whole HTML document, sent alongside `text` when set.
    pub html: Option<String>,
    /// Images the HTML shows, sent along with it. Ignored without HTML.
    pub images: Vec<InlineImage>,
    /// The `Message-ID` this replies to, without angle brackets.
    pub in_reply_to: Option<String>,
    /// The thread this belongs to, oldest first, without angle brackets.
    pub references: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_labels_show_their_levels() {
        let mailbox = |name: &str, role| Mailbox {
            name: name.to_owned(),
            delimiter: Some("/".to_owned()),
            role,
        };
        assert_eq!(mailbox("INBOX", Role::Inbox).label(), "Inbox");
        assert_eq!(
            mailbox("[Gmail]/Sent Mail", Role::Sent).label(),
            "[Gmail] / Sent Mail"
        );
        assert_eq!(mailbox("Receipts", Role::Other).label(), "Receipts");
        assert_eq!(
            mailbox("R&-D/R&AOk-unions", Role::Other).label(),
            "R&D / Réunions"
        );
        // A server that doesn't encode names shows them as they are.
        assert_eq!(mailbox("R&D-2026", Role::Other).label(), "R&D-2026");
    }
}
