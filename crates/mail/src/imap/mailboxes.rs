//! Listing an account's mailboxes, and telling which is for what.

use async_imap::types::NameAttribute;
use futures::TryStreamExt as _;
use postden_core::message::{Mailbox, Role};

use super::connection::Session;
use super::watch::with_timeout;
use crate::Result;

/// What a Sent mailbox is called when the server doesn't mark one, in order of preference, compared
/// without regard to case against a mailbox's whole name and then its last part.
const SENT_NAMES: [&str; 4] = ["Sent", "Sent Items", "Sent Messages", "Sent Mail"];
/// What an Archive mailbox is called when the server doesn't mark one, as [`SENT_NAMES`].
const ARCHIVE_NAMES: [&str; 2] = ["Archive", "Archives"];
/// What a Trash mailbox is called when the server doesn't mark one, as [`SENT_NAMES`].
const TRASH_NAMES: [&str; 3] = ["Trash", "Deleted Items", "Deleted Messages"];
/// What a Junk mailbox is called when the server doesn't mark one, as [`SENT_NAMES`].
const JUNK_NAMES: [&str; 5] = ["Junk", "Spam", "Junk E-mail", "Junk Email", "Bulk Mail"];
/// What a Snoozed mailbox is called when the server doesn't mark one, as [`SENT_NAMES`], and what
/// one is created as when there's none.
pub const SNOOZED_NAME: &str = "Snoozed";
/// The special use some servers mark a Snoozed mailbox with (draft-ietf-mailmaint-imap-snooze).
const SNOOZED_ATTRIBUTE: &str = "\\Snoozed";

/// Every mailbox on the account that can be opened, with the inbox, Sent, Archive, Trash,
/// Snoozed and Junk mailboxes marked.
///
/// # Errors
///
/// Returns an error when the mailboxes can't be listed.
pub(crate) async fn list_mailboxes(session: &mut Session) -> Result<Vec<Mailbox>> {
    let names: Vec<_> =
        with_timeout(async { session.list(Some(""), Some("*")).await?.try_collect().await })
            .await?;
    let listed: Vec<Listed> = names
        .iter()
        .map(|n| {
            let attributes = n.attributes();
            let marks = [
                (NameAttribute::Sent, Mark::Sent),
                (NameAttribute::Archive, Mark::Archive),
                (NameAttribute::All, Mark::All),
                (NameAttribute::Trash, Mark::Trash),
                (NameAttribute::Junk, Mark::Junk),
            ];
            Listed {
                name: n.name().to_owned(),
                delimiter: n.delimiter().map(str::to_owned),
                marks: marks
                    .into_iter()
                    .filter(|(attribute, _)| attributes.contains(attribute))
                    .map(|(_, mark)| mark)
                    .chain(attributes.iter().any(is_snoozed).then_some(Mark::Snoozed))
                    .collect(),
                selectable: !attributes.contains(&NameAttribute::NoSelect),
            }
        })
        .collect();
    Ok(classify(&listed))
}

/// Whether a LIST attribute marks the Snoozed mailbox. The crate doesn't know `\\Snoozed`, so it
/// comes as an extension; attributes ignore case (RFC 3501 section 9).
fn is_snoozed(attribute: &NameAttribute<'_>) -> bool {
    matches!(attribute, NameAttribute::Extension(a) if a.eq_ignore_ascii_case(SNOOZED_ATTRIBUTE))
}

/// The name of the account's Sent mailbox, if it has one.
///
/// # Errors
///
/// Returns an error when the mailboxes can't be listed.
pub(crate) async fn find_sent(session: &mut Session) -> Result<Option<String>> {
    Ok(list_mailboxes(session)
        .await?
        .into_iter()
        .find(|m| m.role == Role::Sent)
        .map(|m| m.name))
}

/// The selectable mailboxes, each with its role. A mailbox has one role at most: the inbox comes
/// first, then Sent, Archive, Trash, Snoozed and Junk, each picked from the mailboxes not picked
/// already.
fn classify(listed: &[Listed]) -> Vec<Mailbox> {
    // INBOX is case-insensitive (RFC 3501 section 5.1).
    let inbox = listed
        .iter()
        .find(|l| l.name.eq_ignore_ascii_case("INBOX"))
        .map(|l| l.name.as_str());
    let mut taken: Vec<&str> = inbox.into_iter().collect();
    let mut roles = Vec::new();
    // Gmail has no Archive mailbox: archiving there moves mail to All Mail, which it marks `\All`.
    let wanted: [(Role, &[Mark], &[&str]); 5] = [
        (Role::Sent, &[Mark::Sent], &SENT_NAMES),
        (Role::Archive, &[Mark::Archive, Mark::All], &ARCHIVE_NAMES),
        (Role::Trash, &[Mark::Trash], &TRASH_NAMES),
        (Role::Snoozed, &[Mark::Snoozed], &[SNOOZED_NAME]),
        (Role::Junk, &[Mark::Junk], &JUNK_NAMES),
    ];
    for (role, marks, names) in wanted {
        if let Some(name) = pick(listed, marks, names, &taken) {
            taken.push(name);
            roles.push((name, role));
        }
    }
    listed
        .iter()
        .filter(|l| l.selectable)
        .map(|l| Mailbox {
            name: l.name.clone(),
            delimiter: l.delimiter.clone(),
            role: if inbox == Some(l.name.as_str()) {
                Role::Inbox
            } else {
                roles
                    .iter()
                    .find(|(name, _)| *name == l.name)
                    .map_or(Role::Other, |(_, role)| *role)
            },
        })
        .collect()
}

/// A special use the server marks a mailbox with (RFC 6154, and `\\Snoozed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Sent,
    Archive,
    /// Every message on the account, as Gmail's All Mail.
    All,
    Trash,
    Snoozed,
    Junk,
}

/// A mailbox as LIST reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    name: String,
    delimiter: Option<String>,
    marks: Vec<Mark>,
    selectable: bool,
}

/// The first selectable mailbox not `taken` that the server marks with one of `marks`, in their
/// order, or else the first that's named like one of `names`, in theirs.
fn pick<'a>(
    listed: &'a [Listed],
    marks: &[Mark],
    names: &[&str],
    taken: &[&str],
) -> Option<&'a str> {
    let usable = || {
        listed
            .iter()
            .filter(|l| l.selectable && !taken.contains(&l.name.as_str()))
    };
    if let Some(marked) = marks
        .iter()
        .find_map(|mark| usable().find(|l| l.marks.contains(mark)))
    {
        return Some(&marked.name);
    }
    let last_part = |l: &Listed| match l.delimiter.as_deref() {
        Some(d) if !d.is_empty() => l.name.rsplit(d).next().unwrap_or(&l.name).to_owned(),
        _ => l.name.clone(),
    };
    names.iter().find_map(|wanted| {
        usable()
            .find(|l| l.name.eq_ignore_ascii_case(wanted))
            .or_else(|| usable().find(|l| last_part(l).eq_ignore_ascii_case(wanted)))
            .map(|l| l.name.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str, marks: &[Mark]) -> Listed {
        Listed {
            name: name.into(),
            delimiter: Some("/".into()),
            marks: marks.to_vec(),
            selectable: true,
        }
    }

    fn role_of(names: &[Listed], role: Role) -> Option<String> {
        classify(names)
            .into_iter()
            .find(|m| m.role == role)
            .map(|m| m.name)
    }

    fn sent(names: &[Listed]) -> Option<String> {
        role_of(names, Role::Sent)
    }

    #[test]
    fn the_marked_mailbox_wins() {
        let names = [
            listed("INBOX", &[]),
            listed("Sent", &[]),
            listed("[Gmail]/Sent Mail", &[Mark::Sent]),
        ];
        assert_eq!(sent(&names).as_deref(), Some("[Gmail]/Sent Mail"));
    }

    #[test]
    fn without_a_mark_a_familiar_name_is_used() {
        let names = [listed("INBOX", &[]), listed("Sent Items", &[])];
        assert_eq!(sent(&names).as_deref(), Some("Sent Items"));
        let nested = [Listed {
            delimiter: Some(".".into()),
            ..listed("INBOX.Sent", &[])
        }];
        assert_eq!(sent(&nested).as_deref(), Some("INBOX.Sent"));
        assert_eq!(sent(&[listed("INBOX", &[])]), None);
    }

    #[test]
    fn a_mailbox_that_cant_be_selected_is_skipped() {
        let names = [
            Listed {
                selectable: false,
                ..listed("[Gmail]", &[Mark::Sent])
            },
            listed("sent", &[]),
        ];
        assert_eq!(sent(&names).as_deref(), Some("sent"));
    }

    #[test]
    fn archive_and_trash_are_found_by_mark_then_by_name() {
        let marked = [
            listed("INBOX", &[]),
            listed("Archive", &[]),
            listed("Old stuff", &[Mark::Archive]),
            listed("Bin", &[Mark::Trash]),
            listed("Trash", &[]),
        ];
        assert_eq!(
            role_of(&marked, Role::Archive).as_deref(),
            Some("Old stuff")
        );
        assert_eq!(role_of(&marked, Role::Trash).as_deref(), Some("Bin"));

        let named = [
            listed("INBOX", &[]),
            listed("Folders/Archives", &[]),
            listed("Deleted Items", &[]),
        ];
        assert_eq!(
            role_of(&named, Role::Archive).as_deref(),
            Some("Folders/Archives")
        );
        assert_eq!(
            role_of(&named, Role::Trash).as_deref(),
            Some("Deleted Items")
        );

        let neither = [listed("INBOX", &[]), listed("Receipts", &[])];
        assert_eq!(role_of(&neither, Role::Archive), None);
        assert_eq!(role_of(&neither, Role::Trash), None);
    }

    #[test]
    fn gmail_archives_to_all_mail_unless_an_archive_is_marked() {
        let gmail = [
            listed("INBOX", &[]),
            listed("[Gmail]/All Mail", &[Mark::All]),
            listed("[Gmail]/Trash", &[Mark::Trash]),
        ];
        assert_eq!(
            role_of(&gmail, Role::Archive).as_deref(),
            Some("[Gmail]/All Mail")
        );
        let both = [
            listed("All", &[Mark::All]),
            listed("Archive", &[Mark::Archive]),
        ];
        assert_eq!(role_of(&both, Role::Archive).as_deref(), Some("Archive"));
    }

    #[test]
    fn snoozed_is_found_by_mark_then_by_name() {
        let marked = [
            listed("INBOX", &[]),
            listed("Snoozed", &[]),
            listed("Later", &[Mark::Snoozed]),
        ];
        assert_eq!(role_of(&marked, Role::Snoozed).as_deref(), Some("Later"));
        let named = [listed("INBOX", &[]), listed("INBOX/snoozed", &[])];
        assert_eq!(
            role_of(&named, Role::Snoozed).as_deref(),
            Some("INBOX/snoozed")
        );
        assert_eq!(role_of(&[listed("INBOX", &[])], Role::Snoozed), None);
    }

    #[test]
    fn junk_is_found_by_mark_then_by_name() {
        let marked = [
            listed("INBOX", &[]),
            listed("Junk", &[]),
            listed("[Gmail]/Spam", &[Mark::Junk]),
        ];
        assert_eq!(
            role_of(&marked, Role::Junk).as_deref(),
            Some("[Gmail]/Spam")
        );
        let named = [listed("INBOX", &[]), listed("Junk E-mail", &[])];
        assert_eq!(role_of(&named, Role::Junk).as_deref(), Some("Junk E-mail"));
        assert_eq!(role_of(&[listed("INBOX", &[])], Role::Junk), None);
    }

    #[test]
    fn the_snoozed_attribute_is_read_whatever_its_case() {
        assert!(is_snoozed(&NameAttribute::Extension("\\snoozed".into())));
        assert!(!is_snoozed(&NameAttribute::Extension("\\Junk".into())));
        assert!(!is_snoozed(&NameAttribute::Archive));
    }

    #[test]
    fn a_mailbox_has_one_role_at_most() {
        // Marked both ways: Sent is picked first, so it isn't the archive too.
        let names = [
            listed("INBOX", &[Mark::Archive]),
            listed("Sent", &[Mark::Sent, Mark::Archive]),
        ];
        let roles: Vec<(String, Role)> = classify(&names)
            .into_iter()
            .map(|m| (m.name, m.role))
            .collect();
        assert_eq!(
            roles,
            [
                ("INBOX".to_owned(), Role::Inbox),
                ("Sent".to_owned(), Role::Sent),
            ]
        );
    }

    #[test]
    fn every_openable_mailbox_is_listed_with_its_role() {
        let names = [
            listed("INBOX", &[]),
            Listed {
                selectable: false,
                ..listed("[Gmail]", &[])
            },
            listed("[Gmail]/Sent Mail", &[Mark::Sent]),
            listed("Receipts", &[]),
        ];
        let roles: Vec<(String, Role)> = classify(&names)
            .into_iter()
            .map(|m| (m.name, m.role))
            .collect();
        assert_eq!(
            roles,
            [
                ("INBOX".to_owned(), Role::Inbox),
                ("[Gmail]/Sent Mail".to_owned(), Role::Sent),
                ("Receipts".to_owned(), Role::Other),
            ]
        );
    }
}
