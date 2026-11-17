//! IMAP: connecting, fetching message headers, watching the inbox for new mail, refreshing the
//! other mailboxes on a timer, opening, downloading, flagging and moving single messages, and
//! filing sent mail.
//!
//! A message is snoozed by moving it to the Snoozed mailbox, and brought back by its Message-ID.
//!
//! Messages can be downloaded ahead of their opening, so they open without waiting.
//! Mail older than what a sync fetched is fetched on demand, as the list is scrolled to its end.
//! A search on the server finds what the cache can't: older mail, and words in message bodies.

mod connection;
mod envelope;
mod mailboxes;
mod message;
mod older;
mod refresh;
mod search;
mod sent;
mod watch;

pub use connection::{Session, check_login, connect};
pub use mailboxes::SNOOZED_NAME;
pub use message::{
    Prefetched, download_message, mark_all_seen, move_message, open_message, prefetch_messages,
    restore_message, set_flagged, set_seen, snooze_message, wake_message,
};
pub use older::{Older, fetch_older};
pub use refresh::refresh;
pub use search::{Found, search};
pub use sent::save_sent_copy;
pub use watch::{Changes, Event, Flags, Status, WatchConfig, watch};
