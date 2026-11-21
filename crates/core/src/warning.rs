//! Signs that a message isn't what it claims to be.
//!
//! A sender the account's own mail server couldn't verify, a name or domain made to look like
//! another, a link going somewhere else than it says, an attachment that can run code.
//!
//! Everything is found on this computer, from the message itself and the contacts written to.
//! Nothing is looked up: checking a signature's key in DNS would tell its sender that, and when,
//! the message was opened.

/// One sign that a message may be phishing, or carry malware.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Warning {
    /// The account's mail server couldn't verify that the message comes from `domain`, the
    /// sender's: its DMARC check failed, or its SPF check did with no DKIM signature passing.
    Unverified { domain: String },
    /// The sender's name is an email address, `shown`, other than the one it was sent from.
    NameHidesAddress { shown: String },
    /// The sender's domain, `domain` as read, is spelled with letters of another alphabet that
    /// look like Latin ones.
    LookalikeSender { domain: String },
    /// Replies go to an address at `domain`, a free mail provider, rather than to the sender's
    /// own domain.
    RepliesToFreemail { domain: String },
    /// A link's text is the address `shown`, but it goes to `target`.
    MisleadingLink { shown: String, target: String },
    /// A link goes to a bare IP address, `target`, instead of a named site.
    LinkToIp { target: String },
    /// A link's address starts with `shown`, but what follows its `@`, `target`, is where it goes.
    LinkHidesHost { shown: String, target: String },
    /// A link goes to `target`, spelled with letters of another alphabet that look like Latin ones.
    LookalikeLink { target: String },
    /// Attachment `name` can run code, or show a fake sign-in page, once opened.
    RiskyAttachment { name: String },
    /// Attachment `name` hides what kind of file it is: a second extension, or characters that
    /// show its name backwards.
    DisguisedAttachment { name: String },
    /// The sender has the name of a contact, `name`, but mail went to them at `email` instead.
    ImpersonatesContact { name: String, email: String },
    /// The sender's domain, `domain`, is a letter away from `contact`'s, a domain mail went to.
    LookalikeContact { domain: String, contact: String },
    /// Mail never went to the sender. On its own, that's no sign of anything.
    FirstMessage,
}

impl Warning {
    /// What it means, in a sentence, to show.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Unverified { domain } => format!(
                "Your mail server couldn't verify that this message comes from {domain}. It may be forged."
            ),
            Self::NameHidesAddress { shown } => format!(
                "The sender's name says {shown}, but the message was sent from another address."
            ),
            Self::LookalikeSender { domain } => format!(
                "The sender's domain, {domain}, is spelled with letters from another alphabet that look like familiar ones."
            ),
            Self::RepliesToFreemail { domain } => format!(
                "Replies go to a personal {domain} address, not to the sender's own domain."
            ),
            Self::MisleadingLink { shown, target } => {
                format!("A link shows {shown} but goes to {target}.")
            }
            Self::LinkToIp { target } => {
                format!("A link goes to {target}, a bare IP address instead of a named site.")
            }
            Self::LinkHidesHost { shown, target } => {
                format!("A link seems to go to {shown}, but goes to {target}, written after its @.")
            }
            Self::LookalikeLink { target } => format!(
                "A link goes to {target}, spelled with letters from another alphabet that look like familiar ones."
            ),
            Self::RiskyAttachment { name } => format!(
                "The attachment {name} can run programs, or show a fake sign-in page, when opened."
            ),
            Self::DisguisedAttachment { name } => {
                format!("The attachment {name} hides what kind of file it is.")
            }
            Self::ImpersonatesContact { name, email } => format!(
                "The sender is named {name}, like a contact of yours, but you wrote to them at {email}, not this address."
            ),
            Self::LookalikeContact { domain, contact } => format!(
                "The sender's domain, {domain}, looks like {contact}, a domain you've written to."
            ),
            Self::FirstMessage => "You've never written to this sender.".to_owned(),
        }
    }
}

/// Free mail providers: anyone can have an address there, so mail from a company has no reason
/// to want replies at one.
const FREEMAIL: &[&str] = &[
    "aol.com",
    "gmail.com",
    "googlemail.com",
    "gmx.com",
    "gmx.de",
    "gmx.net",
    "hotmail.com",
    "icloud.com",
    "live.com",
    "mail.com",
    "mail.ru",
    "me.com",
    "msn.com",
    "outlook.com",
    "proton.me",
    "protonmail.com",
    "yahoo.com",
    "yandex.com",
    "yandex.ru",
    "zoho.com",
];

/// Whether `domain` is a free mail provider's, where anyone can have an address.
#[must_use]
pub fn is_freemail(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    FREEMAIL.contains(&domain.as_str())
}

/// Second-level labels that countries' registries hand out under, as in `example.co.uk`.
const SECOND_LEVEL: &[&str] = &[
    "ac", "co", "com", "edu", "go", "gov", "ne", "net", "or", "org",
];

/// The part of `host` its owner registered, lowercased.
///
/// `mail.example.com` is `example.com`, and `shop.example.co.uk` is `example.co.uk`. A guess,
/// without the public suffix list, that holds for the domains mail mostly comes from.
#[must_use]
pub fn org_domain(host: &str) -> String {
    let host = host.trim_end_matches('.').to_lowercase();
    let labels: Vec<&str> = host.rsplit('.').collect();
    let keep = match labels.as_slice() {
        [tld, second, _, ..] if tld.len() == 2 && SECOND_LEVEL.contains(second) => 3,
        _ => 2,
    };
    let mut kept: Vec<&str> = labels.into_iter().take(keep).collect();
    kept.reverse();
    kept.join(".")
}

/// The alphabets whose letters are mistaken for each other's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Greek,
    Cyrillic,
    Other,
}

/// The alphabet letter `c` is from, or `None` for digits, hyphens and the like.
fn script(c: char) -> Option<Script> {
    if c.is_ascii_alphabetic() || (('\u{c0}'..='\u{24f}').contains(&c) && c.is_alphabetic()) {
        Some(Script::Latin)
    } else if ('\u{370}'..='\u{3ff}').contains(&c) {
        Some(Script::Greek)
    } else if ('\u{400}'..='\u{52f}').contains(&c) {
        Some(Script::Cyrillic)
    } else if c.is_alphabetic() {
        Some(Script::Other)
    } else {
        None
    }
}

/// Greek and Cyrillic letters that look like Latin ones in a domain.
const LOOK_LATIN: &str = "αικνορυχасеһіјӏоԛрѕԝхуԁ";

/// Whether `label`, one part of a domain, reads as Latin letters it isn't: it mixes Latin with
/// Greek or Cyrillic, or is written only in Greek or Cyrillic letters that look Latin.
fn looks_latin(label: &str) -> bool {
    let scripts: Vec<Script> = label.chars().filter_map(script).collect();
    let has = |s: Script| scripts.contains(&s);
    let mixes = has(Script::Latin) && (has(Script::Greek) || has(Script::Cyrillic));
    let all_lookalike = !scripts.is_empty()
        && scripts
            .iter()
            .all(|s| matches!(s, Script::Greek | Script::Cyrillic))
        && label
            .chars()
            .filter(|c| script(*c).is_some())
            .all(|c| LOOK_LATIN.contains(c));
    mixes || all_lookalike
}

/// `domain` as read, lowercased: with its international letters, where it's written in
/// punycode (`xn--…`).
#[must_use]
pub fn readable(domain: &str) -> String {
    idna::domain_to_unicode(domain.trim_end_matches('.'))
        .0
        .to_lowercase()
}

/// `domain` as read, when it's spelled with letters of another alphabet that look Latin, as in
/// `аpple.com` with a Cyrillic `а`. It may be written as punycode (`xn--…`) or as is.
#[must_use]
pub fn lookalike(domain: &str) -> Option<String> {
    let domain = domain.trim_end_matches('.');
    if domain.is_ascii() && !domain.to_ascii_lowercase().contains("xn--") {
        return None;
    }
    let (read, _errors) = idna::domain_to_unicode(domain);
    read.split('.').any(looks_latin).then_some(read)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_domains_drop_subdomains_but_keep_country_second_levels() {
        assert_eq!(org_domain("mail.Example.com."), "example.com");
        assert_eq!(org_domain("example.com"), "example.com");
        assert_eq!(org_domain("shop.example.co.uk"), "example.co.uk");
        assert_eq!(org_domain("example.co.uk"), "example.co.uk");
        assert_eq!(org_domain("a.b.example.de"), "example.de");
        assert_eq!(org_domain("localhost"), "localhost");
    }

    #[test]
    fn lookalike_domains_mix_alphabets_or_only_use_latin_looking_letters() {
        // Cyrillic а in apple.
        assert_eq!(
            lookalike("\u{430}pple.com").as_deref(),
            Some("\u{430}pple.com")
        );
        assert_eq!(
            lookalike("xn--pple-43d.com").as_deref(),
            Some("\u{430}pple.com")
        );
        // All Cyrillic, all Latin-looking: "сосо".
        assert!(lookalike("\u{441}\u{43e}\u{441}\u{43e}.com").is_some());
        assert_eq!(lookalike("paypal.com"), None);
        assert_eq!(lookalike("münchen.de"), None, "Latin with accents");
        assert_eq!(
            lookalike("xn--mnchen-3ya.de"),
            None,
            "the same, as punycode"
        );
        assert_eq!(lookalike("почта.рф"), None, "a Russian word");
        assert_eq!(lookalike("例え.jp"), None);
    }

    #[test]
    fn freemail_is_recognized_in_any_case() {
        assert!(is_freemail("Gmail.com"));
        assert!(!is_freemail("example.com"));
    }
}
