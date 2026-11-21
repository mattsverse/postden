//! Talks to mail servers: IMAP for reading, SMTP for sending, and Google OAuth for Gmail accounts.
//!
//! [`imap::watch`] keeps a mailbox in sync and reports new mail as it arrives, using IMAP IDLE when
//! the server supports it and polling on a timer otherwise. [`smtp::send`] sends a message. Both log
//! in with [`Credentials`]: a password, or OAuth tokens from [`oauth`].

mod auth;
pub mod imap;
pub mod oauth;
pub mod smtp;
mod tls;
mod warnings;

pub use auth::Credentials;

/// Why talking to a mail server failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("couldn't connect to {host}:{port}: {source}")]
    Connect {
        host: String,
        port: u16,
        #[source]
        source: std::io::Error,
    },
    #[error("the server took too long to respond")]
    Timeout,
    #[error("{0:?} isn't a valid server name for TLS")]
    ServerName(String),
    #[error("TLS failed: {0}")]
    Tls(#[source] std::io::Error),
    #[error("couldn't set up TLS: {0}")]
    TlsConfig(#[from] tokio_rustls::rustls::Error),
    #[error("the server closed the connection")]
    Closed,
    #[error("login failed: {0}")]
    Login(#[source] async_imap::error::Error),
    #[error("IMAP error: {0}")]
    Imap(#[from] async_imap::error::Error),
    #[error("the server didn't report a UIDVALIDITY for {0}")]
    NoUidValidity(String),
    /// The mailbox's UIDVALIDITY changed since the message was listed, so its UID means nothing now.
    #[error("{0} changed on the server; wait for it to resync")]
    UidValidityChanged(String),
    #[error("the message is no longer on the server")]
    MessageGone,
    /// The server supports neither MOVE nor UIDPLUS, so a message can't be moved without risking
    /// others.
    #[error("the server can't move messages")]
    CantMove,
    /// The mailbox's name has a `"`, `\` or control character in it. The IMAP parser keeps a
    /// quoted name's escapes, so it can't be told from another mailbox's, and deleting it could
    /// delete that one.
    #[error("{0} can't be deleted here: its name has a quote, backslash or control character")]
    AmbiguousName(String),
    /// The server doesn't list the mailbox as one that can be deleted: it's gone, the inbox, or
    /// marked for special mail such as drafts or sent mail.
    #[error("{name} can't be deleted: {why}")]
    Undeletable { name: String, why: &'static str },
    /// DELETE was sent, but the server didn't say how it went: the mailbox may be gone or not.
    #[error("the server didn't say whether {name} was deleted: {source}")]
    Unconfirmed {
        name: String,
        #[source]
        source: Box<Self>,
    },
    #[error("SMTP error: {0}")]
    Smtp(#[source] lettre::transport::smtp::Error),
    #[error("{0:?} isn't a valid email address")]
    Address(String),
    #[error("the message has no recipients")]
    NoRecipients,
    #[error("couldn't build the message: {0}")]
    Message(String),
    #[error(
        "Google sign-in isn't set up in this build: set POSTDEN_GOOGLE_CLIENT_ID (and \
         POSTDEN_GOOGLE_CLIENT_SECRET) to an OAuth client's"
    )]
    NoOAuthClient,
    #[error("couldn't listen for the browser's sign-in: {0}")]
    Listen(#[source] std::io::Error),
    #[error("couldn't reach Google: {0}")]
    Http(#[source] reqwest::Error),
    #[error("Google sign-in failed: {0}")]
    OAuth(String),
    /// Google no longer accepts the refresh token (revoked, expired, or the password changed).
    #[error("Google refused the sign-in ({0}); sign in with Google again")]
    SignInRefused(String),
}

impl Error {
    /// Whether what was asked may have been done anyway: the server never answered, as when the
    /// connection dropped or timed out once the command was sent, rather than refusing it.
    #[must_use]
    pub const fn may_have_happened(&self) -> bool {
        matches!(
            self,
            Self::Unconfirmed { .. }
                | Self::Timeout
                | Self::Closed
                | Self::Imap(
                    async_imap::error::Error::Io(_) | async_imap::error::Error::ConnectionLost
                )
        )
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
