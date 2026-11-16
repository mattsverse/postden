//! The code the sending computer shows and the receiving one types: 80 random bits, written as
//! 16 Crockford base32 characters in groups of four, like `k3f9-2xqa-7m0d-wnhc`.
//!
//! The sender's iroh key is derived from the code, so the receiver finds the sender from the code
//! alone. So is a token the receiver proves it knows the code with: a relay sees which key it
//! carries traffic for, but not the code.

use std::fmt;
use std::str::FromStr;

use iroh::{EndpointId, SecretKey};
use ring::digest::{Context, SHA256};
use ring::rand::{SecureRandom as _, SystemRandom};

/// Crockford's base32: digits and lowercase letters but `i`, `l`, `o` and `u`, which read as
/// `1`, `1`, `0` and `v`.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// How many random bytes a code holds.
const BYTES: usize = 10;
/// How many characters a code is written with: 5 bits each.
const CHARS: usize = BYTES * 8 / 5;
/// How many characters go between dashes.
const GROUP: usize = 4;

/// A code that finds a sending computer and unlocks what it sends.
#[derive(Clone, PartialEq, Eq)]
pub struct Code([u8; BYTES]);

/// Why a typed code isn't one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "that's not a code Postden shows: it has {CHARS} letters and digits, like k3f9-2xqa-7m0d-wnhc"
)]
pub struct InvalidCode;

impl Code {
    /// A new, random code.
    pub(crate) fn generate() -> Result<Self, ring::error::Unspecified> {
        let mut bytes = [0; BYTES];
        SystemRandom::new().fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// The key the sender's endpoint has.
    pub(crate) fn secret_key(&self) -> SecretKey {
        SecretKey::from_bytes(&self.derive(b"postden transfer key v1"))
    }

    /// Who the receiver dials.
    pub(crate) fn endpoint(&self) -> EndpointId {
        self.secret_key().public()
    }

    /// What the receiver sends first, to show it knows the code.
    pub(crate) fn token(&self) -> [u8; 32] {
        self.derive(b"postden transfer token v1")
    }

    fn derive(&self, purpose: &[u8]) -> [u8; 32] {
        let mut context = Context::new(&SHA256);
        context.update(purpose);
        context.update(&self.0);
        let mut out = [0; 32];
        out.copy_from_slice(context.finish().as_ref());
        out
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut bits = self.0.iter().fold(0_u128, |n, &b| n << 8 | u128::from(b));
        let mut chars = [0_u8; CHARS];
        for c in chars.iter_mut().rev() {
            let digit = usize::try_from(bits & 0x1f).map_err(|_overflow| fmt::Error)?;
            *c = *ALPHABET.get(digit).ok_or(fmt::Error)?;
            bits >>= 5;
        }
        for (i, group) in chars.chunks(GROUP).enumerate() {
            if i > 0 {
                f.write_str("-")?;
            }
            f.write_str(std::str::from_utf8(group).map_err(|_invalid| fmt::Error)?)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Code {
    /// Never the code itself: it's as good as the profile's passphrase-locked file.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Code(…)")
    }
}

impl FromStr for Code {
    type Err = InvalidCode;

    /// Reads a code however it was typed: in either case, with or without dashes and spaces,
    /// and with `o`, `i` and `l` for `0`, `1` and `1`.
    fn from_str(typed: &str) -> Result<Self, InvalidCode> {
        let mut bits = 0_u128;
        let mut count = 0;
        for c in typed.chars().filter(|c| !c.is_whitespace() && *c != '-') {
            let c = match c.to_ascii_lowercase() {
                'o' => '0',
                'i' | 'l' => '1',
                c => c,
            };
            let digit = ALPHABET
                .iter()
                .position(|&a| char::from(a) == c)
                .ok_or(InvalidCode)?;
            bits = bits << 5 | u128::try_from(digit).map_err(|_overflow| InvalidCode)?;
            count += 1;
        }
        if count != CHARS {
            return Err(InvalidCode);
        }
        let mut bytes = [0; BYTES];
        for b in bytes.iter_mut().rev() {
            *b = u8::try_from(bits & 0xff).map_err(|_overflow| InvalidCode)?;
            bits >>= 8;
        }
        Ok(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_reads_back_however_it_was_typed() {
        let code = Code::generate().unwrap();
        let shown = code.to_string();
        assert_eq!(shown.len(), CHARS + CHARS / GROUP - 1, "got {shown}");
        assert_eq!(shown.parse::<Code>().unwrap(), code, "from {shown}");
        let shouted = shown.to_uppercase().replace('-', " ");
        assert_eq!(shouted.parse::<Code>().unwrap(), code, "from {shouted}");

        let known = Code([0xff; BYTES]);
        assert_eq!(known.to_string(), "zzzz-zzzz-zzzz-zzzz");
        let zero = Code([0; BYTES]);
        assert_eq!("OOOO-oooo-0000 0000".parse::<Code>().unwrap(), zero);
        assert_eq!(
            "1111-1111-1111-1111".parse::<Code>(),
            "iIlL-1111-1111-1111".parse::<Code>()
        );
    }

    #[test]
    fn what_isnt_a_code_is_refused() {
        for typed in [
            "",
            "k3f9-2xqa-7m0d",
            "k3f9-2xqa-7m0d-wnhc-a",
            "k3f9-2xqa-7m0d-wnhu",
        ] {
            assert_eq!(typed.parse::<Code>(), Err(InvalidCode), "{typed:?}");
        }
    }

    #[test]
    fn a_code_finds_one_sender_and_never_shows_in_logs() {
        let (one, other) = (Code([1; BYTES]), Code([2; BYTES]));
        assert_eq!(
            one.endpoint(),
            Code([1; BYTES]).endpoint(),
            "the same code, the same sender"
        );
        assert_ne!(
            one.endpoint(),
            other.endpoint(),
            "another code, another sender"
        );
        assert_ne!(one.token(), other.token(), "another code, another token");
        assert_eq!(format!("{one:?}"), "Code(…)");
    }
}
