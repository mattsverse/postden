//! IMAP's modified UTF-7 (RFC 3501 section 5.1.3), how mailbox names are spelled on the wire.
//!
//! Unless UTF-8 is enabled, printable ASCII goes as is but `&` as `&-`, and everything else as
//! `&`, UTF-16 in a base64 with `,` for `/` and no padding, then `-`.

/// The base64 alphabet of modified UTF-7.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

/// `name` in modified UTF-7, as a server takes it.
#[must_use]
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut wide: Vec<u16> = Vec::new();
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut wide, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            wide.extend_from_slice(c.encode_utf16(&mut [0; 2]));
        }
    }
    flush(&mut wide, &mut out);
    out
}

/// Writes `wide`, a run of characters that aren't printable ASCII, as `&…-`, and empties it.
fn flush(wide: &mut Vec<u16>, out: &mut String) {
    if wide.is_empty() {
        return;
    }
    out.push('&');
    let bytes: Vec<u8> = wide.drain(..).flat_map(u16::to_be_bytes).collect();
    let (mut bits, mut held) = (0_u32, 0_u32);
    for byte in bytes {
        bits = (bits << 8) | u32::from(byte);
        held += 8;
        while held >= 6 {
            held -= 6;
            out.push(sextet((bits >> held) & 0x3f));
        }
    }
    if held > 0 {
        out.push(sextet((bits << (6 - held)) & 0x3f));
    }
    out.push('-');
}

/// The character of a 6-bit value.
fn sextet(value: u32) -> char {
    ALPHABET
        .get(usize::try_from(value).unwrap_or(0))
        .map_or('A', |&b| char::from(b))
}

/// `name` as a server spelled it, read from modified UTF-7; `None` if it isn't how modified UTF-7
/// spells anything, such as a raw `R&D-2026` from a server that doesn't encode names.
#[must_use]
pub fn decode(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at)?);
        let after = rest.get(at + 1..)?;
        let end = after.find('-')?;
        let encoded = after.get(..end)?;
        if encoded.is_empty() {
            out.push('&');
        } else {
            let (mut bits, mut held) = (0_u32, 0_u32);
            let mut bytes = Vec::new();
            for b in encoded.bytes() {
                let value = ALPHABET.iter().position(|&a| a == b)?;
                bits = (bits << 6) | u32::try_from(value).ok()?;
                held += 6;
                if held >= 8 {
                    held -= 8;
                    bytes.push(u8::try_from((bits >> held) & 0xff).ok()?);
                }
            }
            if bytes.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_be_bytes(pair))
                .collect();
            out.push_str(&String::from_utf16(&units).ok()?);
        }
        rest = after.get(end + 1..)?;
    }
    out.push_str(rest);
    // Only one spelling is valid: anything else, such as a run that ends mid-character or with
    // bits left over, isn't modified UTF-7 at all.
    (encode(&out) == name).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_go_both_ways() {
        for (plain, wire) in [
            ("Projects", "Projects"),
            ("R&D", "R&-D"),
            ("Réunions", "R&AOk-unions"),
            ("台北", "&U,BTFw-"),
            ("~peter/mail/日本語/台北", "~peter/mail/&ZeVnLIqe-/&U,BTFw-"),
            ("😀 fun", "&2D3eAA- fun"),
        ] {
            assert_eq!(encode(plain), wire, "{plain}");
            assert_eq!(decode(wire).as_deref(), Some(plain), "{wire}");
        }
    }

    #[test]
    fn a_name_that_isnt_modified_utf7_doesnt_decode() {
        // Unterminated, outside the alphabet, too short for a character, bits left over, printable
        // ASCII encoded, and two runs that should be one.
        for raw in ["R&D", "&Jjo!-", "R&D-2026", "&AOl-", "&AGE-", "&AOk-&AOk-"] {
            assert_eq!(decode(raw), None, "{raw}");
        }
    }
}
