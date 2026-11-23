//! What a message is judged by: who sent it and to whom, the words of its subject, and the words
//! its body begins with if it was downloaded, each as a hashed feature.

use std::collections::BTreeSet;

use postden_core::message::Envelope;

/// How much of a body is read, in bytes: where a message says what it's about.
const BODY_BYTES: usize = 4000;
/// How many recipients are features of their own; past that, a message is just sent to many.
const RECIPIENTS: usize = 5;
/// Words shorter or longer than this aren't words to judge by: initials, or links and codes.
const WORD_LEN: std::ops::RangeInclusive<usize> = 2..=24;

/// A feature's hash: FNV-1a, stable across runs and versions, unlike the standard library's.
pub(crate) fn hash(kind: &str, value: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    kind.bytes()
        .chain([0])
        .chain(value.bytes())
        .fold(OFFSET, |h, b| (h ^ u64::from(b)).wrapping_mul(PRIME))
}

/// The lowercase words of `text`: runs of letters and digits, of a usable length. Numbers are
/// all one word, as which number rarely matters.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| WORD_LEN.contains(&w.chars().count()))
        .map(|w| {
            if w.chars().all(|c| c.is_ascii_digit()) {
                "#".to_owned()
            } else {
                w.to_lowercase()
            }
        })
}

/// An address's domain, and the domain it's under, as `mail.shop.example` and `shop.example`.
fn domains(email: &str) -> impl Iterator<Item = &str> {
    let domain = email.rsplit_once('@').map(|(_, d)| d);
    let parent = domain.and_then(|d| {
        let mut dots = d.rmatch_indices('.');
        dots.next();
        dots.next().and_then(|(i, _)| d.get(i + 1..))
    });
    domain.into_iter().chain(parent)
}

/// A message's features, each once, in order. `correspondent` says its sender is someone the
/// account has written to.
pub(crate) fn features(envelope: &Envelope, text: Option<&str>, correspondent: bool) -> Vec<u64> {
    let mut set = BTreeSet::new();
    if let Some(email) = envelope.from.email.as_deref() {
        let email = email.to_lowercase();
        set.insert(hash("from", &email));
        set.extend(domains(&email).map(|d| hash("domain", d)));
    }
    if let Some(name) = envelope.from.name.as_deref() {
        set.extend(words(name).map(|w| hash("name", &w)));
    }
    if let Some(subject) = envelope.subject.as_deref() {
        set.extend(words(subject).map(|w| hash("subject", &w)));
    }
    let to = envelope.to.iter().filter_map(|a| a.email.as_deref());
    set.extend(to.take(RECIPIENTS).map(|e| hash("to", &e.to_lowercase())));
    if envelope.to.len() > 1 {
        set.insert(hash("to", "many"));
    }
    if let Some(text) = text {
        let end = text.floor_char_boundary(BODY_BYTES);
        set.extend(words(text.get(..end).unwrap_or(text)).map(|w| hash("body", &w)));
    }
    if correspondent {
        set.insert(hash("correspondent", ""));
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use postden_core::message::Address;

    use super::*;

    fn envelope(from: &str, subject: &str) -> Envelope {
        Envelope {
            uid: 1,
            message_id: None,
            from: Address {
                name: Some("Shop News".into()),
                email: Some(from.into()),
            },
            to: Vec::new(),
            subject: Some(subject.into()),
            date: None,
            seen: false,
            flagged: false,
            in_reply_to: None,
            references: Vec::new(),
        }
    }

    #[test]
    fn senders_share_their_domain() {
        let a = features(&envelope("deals@mail.shop.example", "Sale"), None, false);
        let b = features(&envelope("news@shop.example", "Hello"), None, false);
        assert!(
            a.contains(&hash("domain", "shop.example")),
            "the domain it's under"
        );
        assert!(
            b.contains(&hash("domain", "shop.example")),
            "its own domain"
        );
        assert!(
            a.contains(&hash("domain", "mail.shop.example")),
            "its own domain"
        );
    }

    #[test]
    fn words_ignore_case_and_which_number() {
        let a = features(&envelope("a@b.example", "Order 1234 SHIPPED"), None, false);
        let b = features(&envelope("a@b.example", "order 99 shipped"), None, false);
        assert_eq!(a, b, "same words, same features");
    }

    #[test]
    fn a_body_adds_its_words_and_a_long_one_is_cut() {
        let without = features(&envelope("a@b.example", "Hi"), None, false);
        let long = format!("invoice {}", "é".repeat(BODY_BYTES));
        let with = features(&envelope("a@b.example", "Hi"), Some(&long), false);
        assert!(with.contains(&hash("body", "invoice")), "its words");
        assert!(with.len() > without.len(), "more to judge by");
    }

    #[test]
    fn writing_to_someone_is_a_feature() {
        let e = envelope("a@b.example", "Hi");
        assert!(features(&e, None, true).contains(&hash("correspondent", "")));
        assert!(!features(&e, None, false).contains(&hash("correspondent", "")));
    }
}
