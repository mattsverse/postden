//! Listing an account's mailboxes, creating and deleting them, and telling which is for what.

use async_imap::types::NameAttribute;
use futures::TryStreamExt as _;
use postden_core::account::Server;
use postden_core::message::{Mailbox, Role};
use postden_core::mutf7;

use super::connection::{Session, connect};
use super::message::logout;
use super::watch::with_timeout;
use crate::{Credentials, Error, Result};

/// What a Sent mailbox is called when the server doesn't mark one, in order of preference, compared
/// without regard to case against a mailbox's whole name and then its last part.
const SENT_NAMES: [&str; 4] = ["Sent", "Sent Items", "Sent Messages", "Sent Mail"];
/// What an Archive mailbox is called when the server doesn't mark one, as [`SENT_NAMES`].
const ARCHIVE_NAMES: [&str; 2] = ["Archive", "Archives"];
/// What a Trash mailbox is called when the server doesn't mark one, as [`SENT_NAMES`].
const TRASH_NAMES: [&str; 3] = ["Trash", "Deleted Items", "Deleted Messages"];
/// What a Junk mailbox is called when the server doesn't mark one, as [`SENT_NAMES`].
const JUNK_NAMES: [&str; 5] = ["Junk", "Spam", "Junk E-mail", "Junk Email", "Bulk Mail"];
/// What a Drafts mailbox is called when the server doesn't mark one, as [`SENT_NAMES`].
const DRAFTS_NAMES: [&str; 2] = ["Drafts", "Draft"];
/// What a Snoozed mailbox is called when the server doesn't mark one, as [`SENT_NAMES`], and what
/// one is created as when there's none.
pub const SNOOZED_NAME: &str = "Snoozed";
/// The special use some servers mark a Snoozed mailbox with (draft-ietf-mailmaint-imap-snooze).
const SNOOZED_ATTRIBUTE: &str = "\\Snoozed";

/// Whether a mailbox is named as one the app keeps mail in when the server marks none.
///
/// That's `name`, its levels split by `delimiter`, such as `Sent` or `Clients/Trash`: which of
/// several so named the app picks can hang on the order the server lists them in, so none of
/// them is deleted.
#[must_use]
pub fn has_kept_name(name: &str, delimiter: Option<&str>) -> bool {
    let last = match delimiter {
        Some(d) if !d.is_empty() => name.rsplit(d).next().unwrap_or(name),
        _ => name,
    };
    [
        &SENT_NAMES[..],
        &ARCHIVE_NAMES,
        &TRASH_NAMES,
        &JUNK_NAMES,
        &DRAFTS_NAMES,
        &[SNOOZED_NAME],
    ]
    .iter()
    .flat_map(|names| names.iter())
    .any(|n| name.eq_ignore_ascii_case(n) || last.eq_ignore_ascii_case(n))
}

/// Every mailbox on the account that can be opened, with the inbox, Sent, Archive, Trash,
/// Snoozed, Junk and Drafts mailboxes marked.
///
/// # Errors
///
/// Returns an error when the mailboxes can't be listed.
pub(crate) async fn list_mailboxes(session: &mut Session) -> Result<Vec<Mailbox>> {
    Ok(classify(&list_all(session).await?))
}

/// Every mailbox on the account, as LIST reports it.
async fn list_all(session: &mut Session) -> Result<Vec<Listed>> {
    let names: Vec<_> =
        with_timeout(async { session.list(Some(""), Some("*")).await?.try_collect().await })
            .await?;
    Ok(names
        .iter()
        .map(|n| listed(n.name(), n.delimiter(), n.attributes()))
        .collect())
}

/// Every mailbox on the account, as LIST reports it once the server says it listed them all: as
/// a plain LIST, which the app's other listings are, and, when the server supports special uses
/// (RFC 6154), as one asking for them, which a plain LIST may leave out. For what's deleted on its
/// say, so refused, with why, when the server doesn't end CAPABILITY or a LIST with OK, or tell
/// what it supports.
async fn list_checked(session: &mut Session, name: &str) -> Result<Vec<Vec<Listed>>> {
    use async_imap::imap_proto::{Capability, MailboxDatum, Response};
    let refuse = |why| Error::Undeletable {
        name: name.to_owned(),
        why,
    };
    let mut told = false;
    let mut special_use = false;
    let ended = run_checked(session, "CAPABILITY", |response| {
        if let Response::Capabilities(all) = response {
            told = true;
            // Capability names ignore case (RFC 3501 section 9).
            special_use |= all
                .iter()
                .any(|c| matches!(c, Capability::Atom(a) if a.eq_ignore_ascii_case("SPECIAL-USE")));
        }
    })
    .await?;
    if !ended || !told {
        return Err(refuse("the server wouldn't say what it supports"));
    }
    let mut commands = vec![r#"LIST "" "*""#];
    if special_use {
        commands.push(r#"LIST "" "*" RETURN (SPECIAL-USE)"#);
    }
    let mut listings = Vec::new();
    for command in commands {
        let mut all = Vec::new();
        let ended = run_checked(session, command, |response| {
            if let Response::MailboxData(MailboxDatum::List(l)) = response {
                all.push(listed(&l.name, l.delimiter.as_deref(), &l.name_attributes));
            }
        })
        .await?;
        if !ended {
            return Err(refuse("the server wouldn't list its mailboxes"));
        }
        listings.push(all);
    }
    Ok(listings)
}

/// The UIDVALIDITY the server gives the mailbox `name`, once it says it told it: none if it
/// didn't.
async fn uid_validity_checked(session: &mut Session, name: &str) -> Result<Option<u32>> {
    use async_imap::imap_proto::{MailboxDatum, Response, StatusAttribute};
    let mut found = None;
    // `name` has no quote, backslash or control character in it, as check_deletable makes sure.
    let ended = run_checked(
        session,
        &format!(r#"STATUS "{name}" (UIDVALIDITY)"#),
        |response| {
            if let Response::MailboxData(MailboxDatum::Status { mailbox, status }) = response
                && mailbox == name
            {
                found = status.iter().find_map(|a| match a {
                    StatusAttribute::UidValidity(v) => Some(*v),
                    _ => None,
                });
            }
        },
    )
    .await?;
    Ok(found.filter(|_| ended))
}

/// Runs `command`, handing each untagged response to `each`, until the server ends it: whether it
/// ended it with OK. An untagged BYE ends it at once, as the server's closing the connection.
async fn run_checked(
    session: &mut Session,
    command: &str,
    mut each: impl FnMut(&async_imap::imap_proto::Response<'_>),
) -> Result<bool> {
    use async_imap::imap_proto::{Response, Status};
    with_timeout(async {
        let tag = session.run_command(command).await?;
        loop {
            let response = session.read_response().await;
            let Some(response) = response.map_err(async_imap::error::Error::from)? else {
                return Err(Error::Closed);
            };
            match response.parsed() {
                Response::Done {
                    tag: done, status, ..
                } if *done == tag => {
                    return Ok(matches!(status, Status::Ok));
                }
                Response::Data {
                    status: Status::Bye,
                    ..
                } => return Err(Error::Closed),
                parsed => each(parsed),
            }
        }
    })
    .await
}

/// A mailbox as LIST reports it: `name` and `delimiter` as it spells them, with `attributes`.
fn listed(name: &str, delimiter: Option<&str>, attributes: &[NameAttribute<'_>]) -> Listed {
    let marks = [
        (NameAttribute::Sent, Mark::Sent),
        (NameAttribute::Archive, Mark::Archive),
        (NameAttribute::All, Mark::All),
        (NameAttribute::Trash, Mark::Trash),
        (NameAttribute::Junk, Mark::Junk),
        (NameAttribute::Drafts, Mark::Drafts),
    ];
    Listed {
        name: name.to_owned(),
        delimiter: delimiter.map(str::to_owned),
        marks: marks
            .into_iter()
            .filter(|(attribute, _)| attributes.contains(attribute))
            .map(|(_, mark)| mark)
            .chain(attributes.iter().any(is_snoozed).then_some(Mark::Snoozed))
            .collect(),
        // A mailbox that doesn't exist can't be selected either (RFC 5258 section 3.4).
        selectable: !attributes.iter().any(|a| {
            *a == NameAttribute::NoSelect
                || matches!(a, NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\NonExistent"))
        }),
        special: attributes.iter().any(|a| !is_plain(a)),
    }
}

/// Whether a LIST attribute says only how a mailbox is laid out or kept track of, not that it's
/// for special mail: anything else, such as `\Drafts` or `\Important` (RFC 8457), is.
fn is_plain(attribute: &NameAttribute<'_>) -> bool {
    const PLAIN: [&str; 5] = [
        "\\HasChildren",
        "\\HasNoChildren",
        "\\Subscribed",
        "\\Remote",
        "\\NonExistent",
    ];
    match attribute {
        NameAttribute::NoInferiors
        | NameAttribute::NoSelect
        | NameAttribute::Marked
        | NameAttribute::Unmarked => true,
        NameAttribute::Extension(a) => PLAIN.iter().any(|p| a.eq_ignore_ascii_case(p)),
        _ => false,
    }
}

/// Whether a LIST attribute marks the Snoozed mailbox. The crate doesn't know `\\Snoozed`, so it
/// comes as an extension; attributes ignore case (RFC 3501 section 9).
fn is_snoozed(attribute: &NameAttribute<'_>) -> bool {
    matches!(attribute, NameAttribute::Extension(a) if a.eq_ignore_ascii_case(SNOOZED_ATTRIBUTE))
}

/// Creates a mailbox called `name` on the account, sent in modified UTF-7 as IMAP spells mailbox
/// names. One that has the server's hierarchy delimiter in it is made inside another, as the
/// server sees fit.
///
/// # Errors
///
/// Returns an error when the connection fails or the server refuses, as when the mailbox exists
/// already or the name isn't one it allows.
pub async fn create_mailbox(server: &Server, credentials: &Credentials, name: &str) -> Result<()> {
    let mut session = connect(server, credentials).await?;
    let result = with_timeout(session.create(mutf7::encode(name))).await;
    logout(session).await;
    result
}

/// Deletes the mailbox `name` from the account, and the mail in it with it.
///
/// `name` is as the server listed it, so already in modified UTF-7; `uid_validity` is its
/// UIDVALIDITY when it was asked to be deleted.
///
/// The server is asked first, as what the app last listed may be out of date: the mailbox must
/// still be listed, for no special mail, and not be one the app keeps mail in, as its Sent or
/// Drafts; and it must still have `uid_validity`. That tells another mailbox renamed or made
/// under its name since, as servers give a new mailbox a new UIDVALIDITY; IMAP lets one keep the
/// old, so it's a check at best.
///
/// # Errors
///
/// Returns [`Error::AmbiguousName`], deleting nothing, when `name` has a `"` or `\` in it, and
/// [`Error::Undeletable`] when the server's answers say it mustn't be deleted. Otherwise returns
/// an error when the connection fails or the server refuses, as when the mailbox has mailboxes
/// inside it that the server won't delete it over.
pub async fn delete_mailbox(
    server: &Server,
    credentials: &Credentials,
    name: &str,
    uid_validity: u32,
) -> Result<()> {
    check_deletable(name)?;
    let mut session = connect(server, credentials).await?;
    let result = async {
        // All of them, as the app picks roles among them; and a name may hold LIST's wildcards.
        let listings = list_checked(&mut session, name).await?;
        let refuse = |why| {
            Err(Error::Undeletable {
                name: name.to_owned(),
                why,
            })
        };
        if let Some(why) = refusal(name, &listings) {
            return refuse(why);
        }
        match uid_validity_checked(&mut session, name).await? {
            Some(found) if found == uid_validity => {}
            Some(_) => return refuse("another mailbox has its name on the server now"),
            None => return refuse("the server couldn't confirm it's the same mailbox"),
        }
        // Refused, nothing was deleted; otherwise, as when the connection dropped, it may have been.
        with_timeout(session.delete(name))
            .await
            .map_err(|e| match e {
                Error::Imap(async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_)) => {
                    e
                }
                e => Error::Unconfirmed {
                    name: name.to_owned(),
                    source: Box::new(e),
                },
            })
    }
    .await;
    logout(session).await;
    result
}

/// Why the mailbox `name` mustn't be deleted, as the server lists every mailbox in each of
/// `listings`: if one doesn't list it, any row for it marks it for special mail, whichever mailbox
/// the app picked for that use, or it's one the app keeps mail in (the inbox, Sent, Drafts…,
/// picked as [`classify`] picks them) by any listing, as it is or with its marks left out, as
/// another listing may leave them out.
fn refusal(name: &str, listings: &[Vec<Listed>]) -> Option<&'static str> {
    let listed_everywhere = listings
        .iter()
        .all(|listed| listed.iter().any(|l| l.name == name && l.selectable));
    if listings.is_empty() || !listed_everywhere {
        return Some("the server doesn't list it");
    }
    if listings
        .iter()
        .flatten()
        .any(|l| l.name == name && l.special)
    {
        return Some("the server keeps it for special mail, such as drafts or sent mail");
    }
    let named = listings
        .iter()
        .flatten()
        .any(|l| l.name == name && has_kept_name(name, l.delimiter.as_deref()));
    if named {
        return Some("the app may keep mail in a mailbox of that name, as Sent or Trash");
    }
    let kept = listings.iter().any(|listed| {
        let unmarked: Vec<Listed> = listed
            .iter()
            .map(|l| Listed {
                marks: Vec::new(),
                ..l.clone()
            })
            .collect();
        [classify(listed), classify(&unmarked)]
            .iter()
            .flatten()
            .any(|m| m.name == name && m.role != Role::Other)
    });
    kept.then_some("the app keeps mail in it, as the inbox, Sent or Drafts")
}

/// Refuses a name with a `"` or `\` in it. A quoted LIST answer such as `"A\"B"` (the mailbox
/// `A"B`) is listed with its escape kept, as `A\"B`, and DELETE escapes it again: the server would
/// delete the mailbox `A\"B` instead, if there is one.
fn check_deletable(name: &str) -> Result<()> {
    if name.contains(['"', '\\']) || name.chars().any(char::is_control) {
        return Err(Error::AmbiguousName(name.to_owned()));
    }
    Ok(())
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
/// first, then Sent, Archive, Trash, Snoozed, Junk and Drafts, each picked from the mailboxes not picked
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
    let wanted: [(Role, &[Mark], &[&str]); 6] = [
        (Role::Sent, &[Mark::Sent], &SENT_NAMES),
        (Role::Archive, &[Mark::Archive, Mark::All], &ARCHIVE_NAMES),
        (Role::Trash, &[Mark::Trash], &TRASH_NAMES),
        (Role::Snoozed, &[Mark::Snoozed], &[SNOOZED_NAME]),
        (Role::Junk, &[Mark::Junk], &JUNK_NAMES),
        (Role::Drafts, &[Mark::Drafts], &DRAFTS_NAMES),
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
    Drafts,
}

/// A mailbox as LIST reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    name: String,
    delimiter: Option<String>,
    marks: Vec<Mark>,
    selectable: bool,
    /// It has an attribute marking it for special mail, as [`is_plain`] tells.
    special: bool,
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
            special: !marks.is_empty(),
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
    fn a_name_with_a_quote_or_backslash_isnt_deleted() {
        assert!(check_deletable("Work").is_ok());
        assert!(check_deletable("R&AOk-unions").is_ok());
        for name in [r#"A\"B"#, r"A\B", r#"A"B"#, "A\r\nB"] {
            assert!(
                matches!(check_deletable(name), Err(Error::AmbiguousName(n)) if n == name),
                "{name}"
            );
        }
    }

    #[test]
    fn only_a_listed_mailbox_for_no_special_mail_is_deleted() {
        let all = [
            listed("INBOX", &[]),
            listed("Sent", &[]),
            listed("Projects/Sent", &[]),
            Listed {
                special: true,
                ..listed("Brouillons", &[])
            },
            listed("Work", &[]),
        ];
        assert_eq!(refusal("Work", &[all.to_vec()]), None);
        assert!(refusal("Gone", &[all.to_vec()]).is_some());
        assert!(refusal("INBOX", &[all.to_vec()]).is_some());
        // Marked, whichever the app picked for the role.
        assert!(refusal("Brouillons", &[all.to_vec()]).is_some());
        // Picked by name, as when the marked one's gone: it's where sent mail is filed now.
        assert!(refusal("Sent", &[all.to_vec()]).is_some());
        // Named as Sent, whichever the app picks.
        assert!(refusal("Projects/Sent", &[all[..3].to_vec()]).is_some());
        assert!(refusal("Projects/Sent", &[all[2..].to_vec()]).is_some());
        // Any row marking it counts, as one sent unasked before the one asked for.
        let twice = [
            listed("Work", &[]),
            Listed {
                special: true,
                ..listed("Work", &[])
            },
        ];
        assert!(refusal("Work", &[twice.to_vec()]).is_some());
        // Unmarked, as another listing may show it, it'd be the one sent mail is filed in.
        let marked_elsewhere = [listed("Envoyés", &[Mark::Sent]), listed("Sent Items", &[])];
        assert!(refusal("Sent Items", &[marked_elsewhere.to_vec()]).is_some());
        assert!(refusal("Notes", &[vec![listed("Notes", &[])]]).is_none());
        // Plain, Archive is marked Sent and the app archives to Archives; asked for special uses,
        // Filed is marked Archive. Either way Archives isn't deleted.
        let plain = vec![
            listed("Archive", &[Mark::Sent]),
            listed("Archives", &[]),
            listed("Filed", &[]),
        ];
        let asked = vec![
            listed("Archive", &[Mark::Sent]),
            listed("Archives", &[]),
            listed("Filed", &[Mark::Archive]),
        ];
        // Named as an archive, it's kept by name too, whichever listing picks it.
        assert!(refusal("Archives", std::slice::from_ref(&asked)).is_some());
        assert!(refusal("Archives", &[plain, asked.clone()]).is_some());
        // Listed by one and not the other, it isn't deleted.
        assert!(refusal("Notes", &[vec![listed("Notes", &[])], asked]).is_some());
        assert!(refusal("Notes", &[]).is_some());
    }

    #[test]
    fn a_mailbox_named_as_one_the_app_keeps_mail_in_isnt_deleted() {
        for (name, delimiter) in [
            ("Sent", None),
            ("Clients/Sent", Some("/")),
            ("INBOX.junk e-mail", Some(".")),
            ("Projects/Snoozed", Some("/")),
        ] {
            assert!(has_kept_name(name, delimiter), "{name}");
        }
        assert!(!has_kept_name("Sent Stuff", Some("/")));
        assert!(!has_kept_name("Clients/Work", Some("/")));
        // Either of two Sent folders the app could pick, whichever comes first.
        let tied = [listed("Projects/Sent", &[]), listed("Clients/Sent", &[])];
        for name in ["Projects/Sent", "Clients/Sent"] {
            assert!(refusal(name, &[tied.to_vec()]).is_some(), "{name}");
        }
    }

    #[test]
    fn only_layout_attributes_are_plain() {
        for plain in [
            NameAttribute::NoInferiors,
            NameAttribute::Marked,
            NameAttribute::Extension("\\hasnochildren".into()),
            NameAttribute::Extension("\\Subscribed".into()),
        ] {
            assert!(is_plain(&plain), "{plain:?}");
        }
        for special in [
            NameAttribute::Drafts,
            NameAttribute::Flagged,
            NameAttribute::All,
            NameAttribute::Extension("\\Important".into()),
            NameAttribute::Extension("\\Snoozed".into()),
        ] {
            assert!(!is_plain(&special), "{special:?}");
        }
    }

    #[test]
    fn drafts_is_found_by_mark_then_by_name() {
        let marked = [
            listed("INBOX", &[]),
            listed("Drafts", &[]),
            listed("Brouillons", &[Mark::Drafts]),
        ];
        assert_eq!(
            role_of(&marked, Role::Drafts).as_deref(),
            Some("Brouillons")
        );
        let named = [listed("INBOX", &[]), listed("INBOX/Drafts", &[])];
        assert_eq!(
            role_of(&named, Role::Drafts).as_deref(),
            Some("INBOX/Drafts")
        );
        assert_eq!(role_of(&[listed("INBOX", &[])], Role::Drafts), None);
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
