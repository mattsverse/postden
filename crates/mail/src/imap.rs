//! IMAP: connecting, fetching message headers, watching the inbox for new mail, refreshing the
//! other mailboxes on a timer, opening, flagging and moving single messages, and filing sent mail.
//!
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
pub use message::{move_message, open_message, set_seen};
pub use older::{Older, fetch_older};
pub use refresh::refresh;
pub use search::{Found, search};
pub use sent::save_sent_copy;
pub use watch::{Event, Status, WatchConfig, watch};
