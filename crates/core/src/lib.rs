//! Domain types and traits shared by every Postden crate. Has no I/O and no UI.

pub mod account;
pub mod message;
pub mod mutf7;
pub mod secrets;
pub mod warning;

/// Errors shared across Postden crates.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not implemented yet")]
    Unimplemented,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
