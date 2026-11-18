//! Contacts: the people mail was sent to, remembered while [`crate::Settings`] says to, and
//! suggested as recipients while one is typed in a message's To, Cc or Bcc.
//!
//! Recipients are typed as a comma-separated list. [`suggestions`] finds the one being typed at
//! the caret and the contacts it could be; [`complete`] puts the chosen one in its place.

use std::ops::Range;

use postden_core::message::Address;
use postden_store::Store;

use crate::Result;

/// How many contacts are suggested at once.
pub const SUGGESTIONS: usize = 6;

/// Remembers that mail went to `recipients` at `at` (Unix seconds).
///
/// # Errors
///
/// Returns an error when the store can't be written. Nothing is remembered in that case.
pub fn remember(store: &mut Store, recipients: &[Address], at: i64) -> Result<()> {
    store.remember_contacts(recipients, at)?;
    Ok(())
}

/// The contacts the recipient being typed in `list` at `caret` (a byte offset) could be.
///
/// Those written to most come first. None are while nothing is typed, or what's typed is a
/// whole address already, and none already in the list are.
///
/// # Errors
///
/// Returns an error when the store can't be read.
pub fn suggestions(store: &Store, list: &str, caret: usize) -> Result<Vec<Address>> {
    let Some(typing) = typing(list, caret) else {
        return Ok(Vec::new());
    };
    let query = typing.query.to_lowercase();
    // The addresses of the other recipients, as written in angle brackets or on their own.
    let listed: Vec<String> = recipients(list)
        .filter(|r| !r.contains(&typing.range.start))
        .filter_map(|r| list.get(r))
        .map(|r| {
            r.rfind('<')
                .and_then(|open| r.get(open + 1..))
                .map_or(r, |inside| inside.split('>').next().unwrap_or_default())
        })
        .map(|email| email.trim().to_lowercase())
        .collect();
    let is_listed = |c: &Address| {
        c.email
            .as_deref()
            .is_some_and(|e| listed.contains(&e.to_lowercase()))
    };
    Ok(store.matching_contacts(|c| matches(c, &query) && !is_listed(c), SUGGESTIONS)?)
}

/// The edit that puts `contact` in place of the recipient being typed in `list` at `caret`: the
/// range to replace, and what to write there, followed by a comma to type the next one after.
#[must_use]
pub fn complete(list: &str, caret: usize, contact: &Address) -> (Range<usize>, String) {
    let range = typing(list, caret).map_or(caret..caret, |t| t.range);
    let before = list.get(..range.start).unwrap_or_default();
    // A recipient after a comma starts after a space.
    let space = if before.trim_end().is_empty() || before.ends_with(' ') {
        ""
    } else {
        " "
    };
    // The comma that ended the one replaced, and the spaces around it, are written anew.
    let after = list.get(range.end..).unwrap_or_default();
    let rest = after
        .trim_start()
        .strip_prefix(',')
        .unwrap_or(after)
        .trim_start();
    let end = range.end + (after.len() - rest.len());
    (range.start..end, format!("{space}{}, ", address(contact)))
}

/// The recipient being typed in a comma-separated list.
#[derive(Debug, PartialEq, Eq)]
struct Typing<'a> {
    /// All of it, from after the comma before the caret to the next comma, leading spaces aside.
    range: Range<usize>,
    /// What's typed of it, up to the caret.
    query: &'a str,
}

/// Where each recipient of a comma-separated list is, commas included at their ends. Commas
/// inside a quoted name, which may hold escaped quotes, don't separate recipients.
fn recipients(list: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let (mut quoted, mut escaped) = (false, false);
    let commas = list.char_indices().filter_map(move |(i, ch)| {
        match ch {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => return Some(i),
            _ => {}
        }
        None
    });
    let mut start = 0;
    commas.chain(std::iter::once(list.len())).map(move |comma| {
        let recipient = start..comma;
        start = comma + 1;
        recipient
    })
}

/// The recipient being typed at `caret`, unless it's blank or a whole address already, with
/// its `<email>`.
fn typing(list: &str, caret: usize) -> Option<Typing<'_>> {
    if !list.is_char_boundary(caret) {
        return None;
    }
    let at = recipients(list).find(|r| r.contains(&caret) || r.end == caret)?;
    let lead = list.get(at.start..caret)?;
    let start = at.start + (lead.len() - lead.trim_start().len());
    let query = list.get(start..caret)?.trim_end();
    let whole = list.get(start..at.end)?;
    if query.is_empty() || whole.contains('<') {
        return None;
    }
    Some(Typing {
        range: start..start + whole.trim_end().len(),
        query,
    })
}

/// Whether `contact` could be what's typed, lowercased: its address or name starts with it, or
/// a word of its name does.
fn matches(contact: &Address, query: &str) -> bool {
    let email = contact.email.as_deref().unwrap_or_default().to_lowercase();
    if email.starts_with(query) {
        return true;
    }
    let name = contact.name.as_deref().unwrap_or_default().to_lowercase();
    name.starts_with(query) || name.split_whitespace().any(|w| w.starts_with(query))
}

/// `contact` as typed in a list of recipients, its name quoted if a list would misread it.
fn address(contact: &Address) -> String {
    let email = contact.email.as_deref().unwrap_or_default();
    match contact.name.as_deref().filter(|n| !n.trim().is_empty()) {
        None => email.to_owned(),
        Some(name) if name.contains(|c: char| "()<>[]:;@\\,.\"".contains(c)) => {
            let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\" <{email}>")
        }
        Some(name) => format!("{name} <{email}>"),
    }
}

#[cfg(test)]
mod tests {
    use postden_mail::smtp::parse_addresses;

    use super::*;

    fn contact(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_owned),
            email: Some(email.to_owned()),
        }
    }

    fn store() -> Store {
        let mut store = Store::open_in_memory().unwrap();
        let ada = contact(Some("Ada Lovelace"), "ada@example.com");
        remember(
            &mut store,
            &[ada.clone(), contact(None, "adam@example.org")],
            1,
        )
        .unwrap();
        remember(
            &mut store,
            &[ada, contact(Some("Bob"), "bob@example.com")],
            2,
        )
        .unwrap();
        store
    }

    #[test]
    fn the_recipient_typed_is_the_one_at_the_caret() {
        let list = "ada@example.com, bo, \"Doe, Jo\" <jo@x.com>, ca";
        let bo = list.find("bo").unwrap();
        assert_eq!(
            typing(list, bo + 2),
            Some(Typing {
                range: bo..bo + 2,
                query: "bo"
            })
        );
        assert_eq!(
            typing(list, bo + 1).unwrap().query,
            "b",
            "typed up to the caret"
        );
        assert_eq!(typing(list, list.len()).unwrap().query, "ca");
        let doe = list.find("Doe").unwrap();
        assert_eq!(
            typing(list, doe + 6),
            None,
            "a whole address, comma and all"
        );
        assert_eq!(typing("ada, ", 5), None, "nothing typed yet");
        assert_eq!(typing("", 0), None);
        assert_eq!(typing("é", 1), None, "not a character boundary");
    }

    #[test]
    fn contacts_are_suggested_by_address_or_any_word_of_their_name() {
        let store = store();
        let emails = |list: &str| {
            suggestions(&store, list, list.len())
                .unwrap()
                .into_iter()
                .filter_map(|c| c.email)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            emails("Ad"),
            ["ada@example.com", "adam@example.org"],
            "written to most first"
        );
        assert_eq!(emails("love"), ["ada@example.com"], "a word of the name");
        assert_eq!(emails("bob@example.com, ADA@"), ["ada@example.com"]);
        assert_eq!(
            emails("ada@example.com, ad"),
            ["adam@example.org"],
            "not one already listed"
        );
        assert_eq!(
            emails("Ada <ADA@example.com>, ad"),
            ["adam@example.org"],
            "however it's written"
        );
        assert_eq!(
            emails("xada@example.com, ad"),
            ["ada@example.com", "adam@example.org"],
            "a longer address isn't the same one"
        );
        assert_eq!(emails("zed"), Vec::<String>::new());
        assert_eq!(emails(""), Vec::<String>::new());
    }

    /// `list` once `contact` is completed at `caret`, and where the caret goes after it.
    fn completed(list: &str, caret: usize, contact: &Address) -> (String, usize) {
        let (range, written) = complete(list, caret, contact);
        let mut list = list.to_owned();
        list.replace_range(range.clone(), &written);
        (list, range.start + written.len())
    }

    #[test]
    fn completing_puts_the_contact_in_place_of_what_was_typed() {
        let ada = contact(Some("Ada Lovelace"), "ada@example.com");
        assert_eq!(
            completed("ad", 2, &ada),
            ("Ada Lovelace <ada@example.com>, ".into(), 32)
        );
        let (list, caret) = completed("bob@example.com,ad", 18, &ada);
        assert_eq!(list, "bob@example.com, Ada Lovelace <ada@example.com>, ");
        assert_eq!(caret, list.len());
        let (list, caret) = completed("ad, bob@example.com", 1, &ada);
        assert_eq!(
            list, "Ada Lovelace <ada@example.com>, bob@example.com",
            "what follows stays, behind one comma"
        );
        assert_eq!(list.get(caret..), Some("bob@example.com"));

        let doe = contact(Some("Doe, \"Jo\""), "jo@example.com");
        let (list, _) = completed("jo", 2, &doe);
        assert_eq!(list, "\"Doe, \\\"Jo\\\"\" <jo@example.com>, ");
        assert_eq!(
            parse_addresses(list.trim_end_matches([',', ' '])).unwrap(),
            vec![doe],
            "a name with a comma reads back whole"
        );
        assert_eq!(
            completed("ad", 2, &contact(None, "adam@example.org")).0,
            "adam@example.org, "
        );
    }

    #[test]
    fn an_escaped_quote_in_a_name_doesnt_hide_the_recipients_after_it() {
        let list = "\"ACME 24\\\" Screens\" <acme@example.com>, bo";
        assert_eq!(typing(list, list.len()).unwrap().query, "bo");
    }
}
