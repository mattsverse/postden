//! The [`Warning`]s a message gives of itself, read as it's downloaded: whether the account's
//! mail server verified its sender, and what its sender's name, its replies, its links and its
//! attachments show.
//!
//! The server's verdict is read from the `Authentication-Results` header it added (RFC 8601),
//! never checked again here: that would look up the sender's key in DNS, under a name the sender
//! picks, telling them the message was opened. Anyone can write that header in the mail they
//! send, so only failures count, and only the topmost header's, which the receiving server adds
//! above any that came with the message, and those from the same server: a forged one claiming a
//! pass hides nothing more than a server adding none.

use mail_parser::{Message, MessagePart};
use postden_core::message::Address;
use postden_core::warning::{Warning, is_freemail, lookalike, org_domain};
use url::{Host, Url};

/// The warnings `message`, with `html` as its HTML body, gives of itself, each once.
pub(crate) fn find(
    message: &Message<'_>,
    from: Option<&Address>,
    reply_to: &[Address],
    html: Option<&str>,
) -> Vec<Warning> {
    let mut found = Vec::new();
    let mut add = |w: Warning| {
        if !found.contains(&w) {
            found.push(w);
        }
    };
    let from_domain = from.and_then(|a| a.email.as_deref()).and_then(domain_of);
    if let Some(domain) = &from_domain {
        if server_failed(&authentication_results(message)) {
            add(Warning::Unverified {
                domain: domain.clone(),
            });
        }
        if let Some(read) = lookalike(domain) {
            add(Warning::LookalikeSender { domain: read });
        }
        for to in reply_to.iter().filter_map(|a| a.email.as_deref()) {
            if let Some(d) = domain_of(to)
                && is_freemail(&d)
                && org_domain(&d) != org_domain(domain)
            {
                add(Warning::RepliesToFreemail { domain: d });
            }
        }
    }
    if let Some(shown) = from.and_then(name_hiding_address) {
        add(Warning::NameHidesAddress { shown });
    }
    for link in html.map(postden_html::links).unwrap_or_default() {
        for w in link_warnings(&link) {
            add(w);
        }
    }
    for part in message.attachments() {
        if let Some(w) = attachment_warning(&part) {
            add(w);
        }
    }
    found
}

/// The domain of `email`, lowercased, if it has one.
fn domain_of(email: &str) -> Option<String> {
    let (_, domain) = email.trim().rsplit_once('@')?;
    let domain = domain.trim_end_matches('>').trim().to_lowercase();
    (!domain.is_empty()).then_some(domain)
}

/// One `Authentication-Results` header: which server added it, if it says, and each method's
/// result, such as `("dmarc", "fail")`, lowercased.
#[derive(Debug, PartialEq, Eq)]
struct AuthResults {
    server: Option<String>,
    results: Vec<(String, String)>,
}

/// The `Authentication-Results` headers of `message`, topmost first.
fn authentication_results(message: &Message<'_>) -> Vec<AuthResults> {
    message
        .headers()
        .all("Authentication-Results")
        .map(|h| parse_auth_results(&String::from_utf8_lossy(h.raw_value())))
        .collect()
}

/// `value` without its comments, `(…)`, which may nest, and with its quoted strings emptied:
/// `"…"`. Neither says what a check found, and either may hold a `(`, `)` or `;` that would
/// read as more than it is, such as a sender's address `"x;dkim=pass"@evil.test`.
fn without_comments(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for c in value.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' if depth > 0 || quoted => escaped = true,
            '"' if depth == 0 => {
                quoted = !quoted;
                out.push(c);
            }
            _ if quoted => {}
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Reads an `Authentication-Results` value: `server; method=result …; …`. Some servers, such as
/// Microsoft's, leave the server out and start with a result.
fn parse_auth_results(value: &str) -> AuthResults {
    let value = without_comments(value);
    let mut parts = value.split(';').map(str::trim);
    let first = parts.next().unwrap_or_default();
    let (server, first_result) = if first.contains('=') {
        (None, Some(first))
    } else {
        let server = first.split_whitespace().next().map(str::to_lowercase);
        (server, None)
    };
    let results = first_result
        .into_iter()
        .chain(parts)
        .filter_map(|part| {
            let (method, rest) = part.split_once('=')?;
            let result = rest.split_whitespace().next()?;
            Some((method.trim().to_lowercase(), result.to_lowercase()))
        })
        .collect();
    AuthResults { server, results }
}

/// Whether the receiving server couldn't verify the sender: DMARC failed, or with no DMARC
/// result, SPF failed and no DKIM signature passed.
///
/// A failure counts from the topmost of `headers` or another naming the same server: forged, it
/// only casts doubt on its own sender's mail. What would excuse a failure counts only from the
/// topmost, as a sender can write any other, under any server's name.
fn server_failed(headers: &[AuthResults]) -> bool {
    let Some(top) = headers.first() else {
        return false;
    };
    let failed = |method: &str| {
        headers
            .iter()
            .filter(|h| top.server.is_some() && h.server == top.server)
            .chain(std::iter::once(top))
            .flat_map(|h| &h.results)
            .any(|(m, r)| m == method && r == "fail")
    };
    let top_says = |method: &str, result: Option<&str>| {
        top.results
            .iter()
            .any(|(m, r)| m == method && result.is_none_or(|result| r == result))
    };
    failed("dmarc")
        || (!top_says("dmarc", None) && failed("spf") && !top_says("dkim", Some("pass")))
}

/// The email address in `from`'s name, when it's another than the one it was sent from, as in
/// `"service@paypal.com" <x@evil.test>`.
fn name_hiding_address(from: &Address) -> Option<String> {
    let name = from.name.as_deref()?;
    let email = from.email.as_deref().unwrap_or_default().trim();
    name.split(|c: char| c.is_whitespace() || "<>()\"',;[]".contains(c))
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()))
        .find(|t| {
            t.split_once('@')
                .is_some_and(|(user, host)| !user.is_empty() && host.contains('.'))
                && !t.eq_ignore_ascii_case(email)
        })
        .map(str::to_owned)
}

/// The host a link's text shows, if it reads as a web address, as `paypal.com` or
/// `https://www.paypal.com/login` do; `None` for any other text.
fn shown_host(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let lower = text.to_lowercase();
    let rest = ["https://", "http://"]
        .iter()
        .find_map(|scheme| lower.strip_prefix(scheme))
        .unwrap_or(&lower);
    let host = rest
        .split(['/', '?', '#', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    let labels: Vec<&str> = host.split('.').collect();
    let tld = labels.last()?;
    let reads_as_host = labels.len() >= 2
        && labels
            .iter()
            .all(|l| !l.is_empty() && l.chars().all(|c| c.is_alphanumeric() || c == '-'))
        && tld.len() >= 2
        && tld.chars().all(char::is_alphabetic);
    reads_as_host.then(|| host.to_owned())
}

/// `host` spelled as a link's address is, with international letters as punycode.
fn as_ascii(host: &str) -> Option<String> {
    Url::parse(&format!("http://{host}/"))
        .ok()?
        .host_str()
        .map(str::to_owned)
}

/// What `link` shows of itself that it shouldn't: going elsewhere than its text says, to a bare
/// IP address, to a lookalike domain, or somewhere hidden after an `@`.
fn link_warnings(link: &postden_html::Link) -> Vec<Warning> {
    let Ok(url) = Url::parse(&link.href) else {
        return Vec::new();
    };
    if !matches!(url.scheme(), "http" | "https") {
        return Vec::new();
    }
    let mut found = Vec::new();
    let target = match url.host() {
        Some(Host::Ipv4(ip)) => {
            found.push(Warning::LinkToIp {
                target: ip.to_string(),
            });
            return found;
        }
        Some(Host::Ipv6(ip)) => {
            found.push(Warning::LinkToIp {
                target: ip.to_string(),
            });
            return found;
        }
        Some(Host::Domain(domain)) => domain.to_owned(),
        None => return found,
    };
    if !url.username().is_empty() {
        found.push(Warning::LinkHidesHost {
            shown: url.username().to_owned(),
            target: target.clone(),
        });
    }
    if let Some(read) = lookalike(&target) {
        found.push(Warning::LookalikeLink { target: read });
    } else if let Some(shown) = shown_host(&link.text)
        && as_ascii(&shown).is_some_and(|ascii| org_domain(&ascii) != org_domain(&target))
    {
        found.push(Warning::MisleadingLink { shown, target });
    }
    found
}

/// Kinds of file that run code, macros or scripts, mount as a disk, or show as a web page, once
/// opened: how malware and fake sign-in pages travel.
const RISKY: &[&str] = &[
    "app",
    "apk",
    "applescript",
    "appx",
    "bat",
    "chm",
    "cmd",
    "com",
    "command",
    "cpl",
    "dll",
    "dmg",
    "docm",
    "dotm",
    "exe",
    "gadget",
    "hta",
    "htm",
    "html",
    "img",
    "iso",
    "jar",
    "js",
    "jse",
    "lnk",
    "msc",
    "msi",
    "msix",
    "one",
    "pif",
    "pkg",
    "potm",
    "ppam",
    "ppsm",
    "pptm",
    "ps1",
    "psm1",
    "py",
    "reg",
    "scf",
    "scpt",
    "scr",
    "sh",
    "shtml",
    "sldm",
    "svg",
    "tool",
    "url",
    "vbe",
    "vbs",
    "vhd",
    "vhdx",
    "wsf",
    "wsh",
    "xhtml",
    "xlam",
    "xlsb",
    "xlsm",
    "xltm",
];

/// Kinds of file a risky one pretends to be, as in `invoice.pdf.exe`.
const HARMLESS: &[&str] = &[
    "csv", "doc", "docx", "gif", "jpeg", "jpg", "mp3", "mp4", "pdf", "png", "ppt", "pptx", "rtf",
    "txt", "xls", "xlsx", "zip",
];

/// Characters that change the direction text shows in, which can show a name's end backwards:
/// `invoice\u{202e}fdp.exe` shows as `invoiceexe.pdf`.
const BIDI_CONTROLS: &[char] = &[
    '\u{061c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}',
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];

/// Whether `part`, an attachment, can run code, or disguises what it is.
fn attachment_warning(part: &MessagePart<'_>) -> Option<Warning> {
    let raw = part.attachment_name()?;
    let name: String = raw.chars().filter(|c| !BIDI_CONTROLS.contains(c)).collect();
    let name = name.trim().to_owned();
    if name.len() != raw.trim().len() {
        return Some(Warning::DisguisedAttachment { name });
    }
    let lower = name.to_lowercase();
    let mut parts = lower.rsplit('.');
    let extension = parts.next()?;
    // Without a dot, there's no extension.
    let before = parts.next()?;
    if !RISKY.contains(&extension) {
        return None;
    }
    // A second extension, after a name.
    if parts.next().is_some() && HARMLESS.contains(&before) {
        Some(Warning::DisguisedAttachment { name })
    } else {
        Some(Warning::RiskyAttachment { name })
    }
}

#[cfg(test)]
mod tests {
    use mail_parser::MessageParser;

    use super::*;

    fn warnings(raw: &str) -> Vec<Warning> {
        let message = MessageParser::default()
            .parse(raw.as_bytes())
            .unwrap_or_else(|| panic!("unparsable test message"));
        let address = |list: Option<mail_parser::AddressList<'_>>| -> Vec<Address> {
            list.map(|l| {
                l.mailboxes()
                    .map(|m| Address {
                        name: m.name().map(str::to_owned),
                        email: m.address().map(str::to_owned),
                    })
                    .collect()
            })
            .unwrap_or_default()
        };
        let from = address(message.from());
        let reply_to = address(message.reply_to());
        let html = message.html_bodies().collect::<Vec<_>>().join("\n");
        find(&message, from.first(), &reply_to, Some(&html))
    }

    #[test]
    fn a_plain_verified_message_has_no_warnings() {
        let raw = "Authentication-Results: mx.google.com; dkim=pass header.i=@example.com; spf=pass; dmarc=pass (p=REJECT) header.from=example.com\r\n\
            From: Ada <ada@example.com>\r\n\
            Reply-To: help@example.com\r\n\
            Content-Type: text/html\r\n\r\n\
            <p><a href='https://www.example.com/x'>example.com</a> <a href='https://t.test/1'>Read more</a></p>\r\n";
        assert_eq!(warnings(raw), Vec::<Warning>::new());
    }

    #[test]
    fn dmarc_failing_on_the_topmost_server_warns_but_forged_passes_dont_hide_it() {
        let raw = "Authentication-Results: mx.example.net; dkim=none; spf=softfail; dmarc=fail header.from=bank.test\r\n\
            Authentication-Results: mx.example.net; dmarc=pass\r\n\
            From: bank@bank.test\r\n\r\nhi\r\n";
        assert_eq!(
            warnings(raw),
            vec![Warning::Unverified {
                domain: "bank.test".into()
            }]
        );
    }

    #[test]
    fn spf_failing_counts_only_without_dmarc_or_a_passing_dkim_signature() {
        let spf_only = "Authentication-Results: spf=fail (sender IP is 1.2.3.4) smtp.mailfrom=bank.test; dkim=none\r\n\
            From: bank@bank.test\r\n\r\nhi\r\n";
        assert_eq!(
            warnings(spf_only).len(),
            1,
            "Microsoft's form, no server named"
        );
        let signed = "Authentication-Results: mx.example.net; spf=fail; dkim=pass header.d=bank.test\r\n\
            From: bank@bank.test\r\n\r\nhi\r\n";
        assert_eq!(warnings(signed), Vec::<Warning>::new());
        let dmarc_passed = "Authentication-Results: mx.example.net; spf=fail; dkim=fail; dmarc=pass\r\n\
            From: bank@bank.test\r\n\r\nhi\r\n";
        assert_eq!(warnings(dmarc_passed), Vec::<Warning>::new());
    }

    #[test]
    fn headers_below_the_topmost_cant_excuse_its_failure() {
        for excuse in [
            "Authentication-Results: mx.example.net; dkim=pass header.d=bank.test",
            "Authentication-Results: mx.example.net; dmarc=pass",
        ] {
            let raw = format!(
                "Authentication-Results: mx.example.net; spf=fail; dkim=none\r\n{excuse}\r\n\
                 From: bank@bank.test\r\n\r\nhi\r\n"
            );
            assert_eq!(warnings(&raw).len(), 1, "{excuse}");
        }
        let unnamed = "Authentication-Results: spf=fail; dkim=none\r\n\
            Authentication-Results: dmarc=pass\r\n\
            From: bank@bank.test\r\n\r\nhi\r\n";
        assert_eq!(
            warnings(unnamed).len(),
            1,
            "no server named, the topmost alone"
        );
    }

    #[test]
    fn a_failure_in_a_header_below_another_servers_is_the_senders_and_ignored() {
        let raw = "Authentication-Results: mx.example.net; dmarc=pass\r\n\
            Authentication-Results: evil.test; dmarc=fail\r\n\
            From: friend@friend.test\r\n\r\nhi\r\n";
        assert_eq!(warnings(raw), Vec::<Warning>::new());
    }

    #[test]
    fn quoted_strings_cant_hide_or_forge_results() {
        let hidden = "Authentication-Results: mx.example.net; dkim=pass header.d=evil.test; \
            spf=pass smtp.mailfrom=\"(\"@evil.test; dmarc=fail\r\n\
            From: bank@bank.test\r\n\r\nhi\r\n";
        assert_eq!(warnings(hidden).len(), 1, "a ( in quotes isn't a comment");
        let forged = "Authentication-Results: mx.example.net; spf=fail \
            smtp.mailfrom=\"x;dkim=pass;\"@evil.test\r\n\
            From: bank@bank.test\r\n\r\nhi\r\n";
        assert_eq!(
            warnings(forged).len(),
            1,
            "a ; in quotes doesn't start a result"
        );
    }

    #[test]
    fn comments_in_results_are_skipped() {
        let parsed = parse_auth_results(
            " mx.google.com (version 1;\r\n\tok); dkim=pass (2048-bit key; unprotected) header.i=@a.test;\r\n spf=pass",
        );
        assert_eq!(parsed.server.as_deref(), Some("mx.google.com"));
        assert_eq!(
            parsed.results,
            vec![
                ("dkim".to_owned(), "pass".to_owned()),
                ("spf".to_owned(), "pass".to_owned())
            ]
        );
    }

    #[test]
    fn sender_names_hiding_addresses_lookalike_domains_and_freemail_replies_warn() {
        let raw = "From: \"service@paypal.com\" <x@xn--pypal-4ve.com>\r\n\
            Reply-To: ceo.private@Gmail.com\r\n\r\nhi\r\n";
        assert_eq!(
            warnings(raw),
            vec![
                Warning::LookalikeSender {
                    domain: "p\u{430}ypal.com".into()
                },
                Warning::RepliesToFreemail {
                    domain: "gmail.com".into()
                },
                Warning::NameHidesAddress {
                    shown: "service@paypal.com".into()
                },
            ]
        );
        let same = "From: \"ada@example.com\" <ada@example.com>\r\n\r\nhi\r\n";
        assert_eq!(
            warnings(same),
            Vec::<Warning>::new(),
            "the name is its own address"
        );
        let personal = "From: Ada <ada@gmail.com>\r\nReply-To: ada@gmail.com\r\n\r\nhi\r\n";
        assert_eq!(warnings(personal), Vec::<Warning>::new());
    }

    #[test]
    fn links_going_elsewhere_than_they_say_warn() {
        let raw = "From: a@shop.test\r\n\
            Content-Type: text/html\r\n\r\n\
            <a href='https://paypa1-login.test/x'>https://www.paypal.com/signin</a>\
            <a href='http://192.168.1.2/login'>Log in</a>\
            <a href='https://paypal.com@evil.test/'>Log in</a>\
            <a href='https://xn--pypal-4ve.com/'>Log in</a>\
            <a href='https://mail.shop.test/x'>www.shop.test</a>\
            <a href='https://münchen.de/'>münchen.de</a>\
            <a href='https://paypa1-login.test/y'>https://www.paypal.com/signin</a>\r\n";
        assert_eq!(
            warnings(raw),
            vec![
                Warning::MisleadingLink {
                    shown: "www.paypal.com".into(),
                    target: "paypa1-login.test".into()
                },
                Warning::LinkToIp {
                    target: "192.168.1.2".into()
                },
                Warning::LinkHidesHost {
                    shown: "paypal.com".into(),
                    target: "evil.test".into()
                },
                Warning::LookalikeLink {
                    target: "p\u{430}ypal.com".into()
                },
            ]
        );
    }

    #[test]
    fn shown_hosts_are_only_read_from_addresses() {
        assert_eq!(shown_host("paypal.com").as_deref(), Some("paypal.com"));
        assert_eq!(
            shown_host("HTTPS://Www.PayPal.com:443/x").as_deref(),
            Some("www.paypal.com")
        );
        assert_eq!(shown_host("Read more"), None);
        assert_eq!(shown_host("v2.0"), None);
        assert_eq!(shown_host("Click here."), None);
    }

    #[test]
    fn risky_and_disguised_attachments_warn() {
        let part = |name: &str| {
            format!(
                "--b\r\nContent-Type: application/octet-stream\r\n\
                 Content-Disposition: attachment; filename=\"{name}\"\r\n\r\nx\r\n"
            )
        };
        let raw = format!(
            "From: a@shop.test\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n{}{}{}{}{}{}--b--\r\n",
            part("report.pdf"),
            part("setup.EXE"),
            part("invoice.pdf.exe"),
            part("Q3.xlsm"),
            part("invoice\u{202e}fdp.exe"),
            part("archive.tar.gz"),
        );
        for name in ["run.command", "run.sh", "run.py", "a.scpt", "invoice.xlsb"] {
            let raw = format!(
                "From: a@shop.test\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n{}--b--\r\n",
                part(name)
            );
            assert_eq!(
                warnings(&raw),
                vec![Warning::RiskyAttachment { name: name.into() }],
                "{name}"
            );
        }
        assert_eq!(
            warnings(&raw),
            vec![
                Warning::RiskyAttachment {
                    name: "setup.EXE".into()
                },
                Warning::DisguisedAttachment {
                    name: "invoice.pdf.exe".into()
                },
                Warning::RiskyAttachment {
                    name: "Q3.xlsm".into()
                },
                Warning::DisguisedAttachment {
                    name: "invoicefdp.exe".into()
                },
            ]
        );
    }
}
