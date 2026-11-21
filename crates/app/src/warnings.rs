//! A message's [`Warning`]s, with those only the contacts written to can tell.
//!
//! A sender who has a contact's name but not their address, a domain a letter away from one mail
//! went to, and a sender mail never went to.
//!
//! Contacts are kept only while the settings say to; without them, a message has only the
//! warnings it gives of itself.

use postden_core::message::Address;
use postden_core::warning::{Warning, is_freemail, org_domain, readable};
use postden_store::Store;

use crate::Result;

/// The warnings of a message from `from`, which gives `found` of itself, with the contacts'.
///
/// The contacts are in `store`. Mail never having gone to the sender is said only beside another
/// warning: on its own, it's no sign of anything.
///
/// # Errors
///
/// Returns an error when the contacts can't be read.
pub fn with_contacts(store: &Store, from: &Address, found: &[Warning]) -> Result<Vec<Warning>> {
    let mut warnings = found.to_vec();
    let Some(email) = from
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    else {
        return Ok(warnings);
    };
    let contacts = store.matching_contacts(|_| true, usize::MAX)?;
    let known = |c: &Address| {
        c.email
            .as_deref()
            .is_some_and(|e| e.eq_ignore_ascii_case(email))
    };
    if contacts.iter().any(known) {
        return Ok(warnings);
    }
    if let Some(name) = from
        .name
        .as_deref()
        .map(normalized)
        .filter(|n| n.chars().count() >= 3)
        && let Some(contact) = contacts
            .iter()
            .find(|c| c.name.as_deref().map(normalized).as_ref() == Some(&name))
        && let Some(contact_email) = &contact.email
    {
        warnings.push(Warning::ImpersonatesContact {
            name: contact.name.clone().unwrap_or_default(),
            email: contact_email.clone(),
        });
    }
    if let Some((_, domain)) = email.rsplit_once('@') {
        let domain = org_domain(&readable(domain));
        let near = contacts
            .iter()
            .filter_map(|c| {
                c.email
                    .as_deref()?
                    .rsplit_once('@')
                    .map(|(_, d)| org_domain(&readable(d)))
            })
            .find(|contact| looks_like(&domain, contact));
        if let Some(contact) = near {
            warnings.push(Warning::LookalikeContact { domain, contact });
        }
    }
    if !warnings.is_empty() {
        warnings.push(Warning::FirstMessage);
    }
    Ok(warnings)
}

/// `name` lowercased, its spaces collapsed, to compare.
fn normalized(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether `domain` looks like `contact`, another domain: a letter added, dropped, swapped or
/// replaced, or spelled with digits or letter pairs that look like its letters (`paypa1`,
/// `rnicrosoft`). Short domains and free mail providers' are left alone: too many are a letter
/// apart.
fn looks_like(domain: &str, contact: &str) -> bool {
    const SHORTEST: usize = 6;
    domain != contact
        && domain.chars().count() >= SHORTEST
        && contact.chars().count() >= SHORTEST
        && !is_freemail(domain)
        && (one_edit_apart(domain, contact) || skeleton(domain) == skeleton(contact))
}

/// `domain` with what looks alike made the same: `0` as `o`, `1` and `i` as `l`, `rn` as `m` and
/// `vv` as `w`.
fn skeleton(domain: &str) -> String {
    domain
        .replace("rn", "m")
        .replace("vv", "w")
        .chars()
        .map(|c| match c {
            '0' => 'o',
            '1' | 'i' => 'l',
            c => c,
        })
        .collect()
}

/// Whether `a` and `b` differ by one letter added, dropped or replaced, or two next to each
/// other swapped.
fn one_edit_apart(first: &str, second: &str) -> bool {
    let first: Vec<char> = first.chars().collect();
    let second: Vec<char> = second.chars().collect();
    if first == second {
        return false;
    }
    // Past what they start with alike, where they differ.
    let alike = first
        .iter()
        .zip(&second)
        .take_while(|(one, other)| one == other)
        .count();
    let (Some(one), Some(other)) = (first.get(alike..), second.get(alike..)) else {
        return false;
    };
    let tail = |s: &[char], n: usize| s.get(n..).map(<[char]>::to_vec);
    let replaced = !one.is_empty() && !other.is_empty() && tail(one, 1) == tail(other, 1);
    let added = tail(other, 1).as_deref() == Some(one);
    let dropped = tail(one, 1).as_deref() == Some(other);
    let swapped = one
        .get(..2)
        .zip(other.get(..2))
        .is_some_and(|(pair, other_pair)| pair.iter().rev().eq(other_pair.iter()))
        && tail(one, 2) == tail(other, 2);
    replaced || added || dropped || swapped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_owned),
            email: Some(email.to_owned()),
        }
    }

    fn store_with(contacts: &[Address]) -> Store {
        let mut store = Store::open_in_memory().unwrap();
        store.remember_contacts(contacts, 1).unwrap();
        store
    }

    #[test]
    fn a_contact_or_a_stranger_alone_adds_nothing() {
        let store = store_with(&[address(Some("Ada Lovelace"), "ada@work.test")]);
        let ada = address(Some("Ada Lovelace"), "ADA@work.test");
        assert_eq!(
            with_contacts(&store, &ada, &[]).unwrap(),
            Vec::<Warning>::new()
        );
        let stranger = address(Some("Bob"), "bob@else.test");
        assert_eq!(
            with_contacts(&store, &stranger, &[]).unwrap(),
            Vec::<Warning>::new(),
            "only beside another warning is a first message said"
        );
        let risky = [Warning::RiskyAttachment {
            name: "a.exe".into(),
        }];
        assert_eq!(
            with_contacts(&store, &stranger, &risky).unwrap(),
            vec![risky[0].clone(), Warning::FirstMessage]
        );
        assert_eq!(
            with_contacts(&store, &ada, &risky).unwrap(),
            risky.to_vec(),
            "a contact's mail isn't a first message"
        );
    }

    #[test]
    fn a_contacts_name_from_another_address_warns() {
        let store = store_with(&[address(Some("Ada Lovelace"), "ada@work.test")]);
        let impostor = address(Some(" ada  lovelace"), "ada.lovelace@gmail.com");
        assert_eq!(
            with_contacts(&store, &impostor, &[]).unwrap(),
            vec![
                Warning::ImpersonatesContact {
                    name: "Ada Lovelace".into(),
                    email: "ada@work.test".into()
                },
                Warning::FirstMessage
            ]
        );
    }

    #[test]
    fn a_domain_a_letter_from_a_contacts_warns() {
        let store = store_with(&[
            address(None, "billing@paypal.com"),
            address(None, "me@gmail.com"),
        ]);
        let warns = |email: &str| {
            with_contacts(&store, &address(None, email), &[])
                .unwrap()
                .into_iter()
                .find_map(|w| match w {
                    Warning::LookalikeContact { domain, contact } => Some((domain, contact)),
                    _ => None,
                })
        };
        assert_eq!(
            warns("x@mail.paypa1.com"),
            Some(("paypa1.com".into(), "paypal.com".into()))
        );
        assert!(warns("x@paypall.com").is_some(), "a letter added");
        assert!(warns("x@pyapal.com").is_some(), "two swapped");
        assert!(warns("x@gmai1.com").is_some());
        assert_eq!(
            warns("x@xn--paypl-0qa.com"),
            Some(("payp\u{e1}l.com".into(), "paypal.com".into())),
            "written in punycode, compared as read"
        );
        assert!(warns("x@payp\u{e1}l.com").is_some());
        assert_eq!(warns("x@paypal.com"), None, "the same domain");
        assert_eq!(warns("x@mail.com"), None, "a free mail provider's");
        assert_eq!(warns("x@example.com"), None);
    }

    #[test]
    fn one_edit_apart_counts_each_kind_of_edit() {
        assert!(one_edit_apart("paypal", "paypa"));
        assert!(one_edit_apart("paypal", "paypall"));
        assert!(one_edit_apart("paypal", "paypol"));
        assert!(one_edit_apart("paypal", "papyal"));
        assert!(!one_edit_apart("paypal", "paypal"));
        assert!(!one_edit_apart("paypal", "pyapla"));
        assert!(!one_edit_apart("paypal", "pay"));
    }
}
