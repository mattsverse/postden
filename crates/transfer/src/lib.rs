//! Sending a Postden profile straight to another computer, over iroh.
//!
//! - [`Offer`]: the sending computer goes online, shows a [`Code`], and hands the profile to the
//!   first computer that comes with it.
//! - [`receive`]: the other computer dials the sender the code names and downloads the profile.
//!
//! The two connect directly when they can, through NATs, and through a relay run by n0 when
//! they can't. Either way the connection is end-to-end encrypted, and the profile still locked
//! with its passphrase: the code only finds and unlocks the transfer.

mod code;

use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{
    ConnectionError, Incoming, ReadError, ReadToEndError, VarInt, WriteError, presets,
};
use iroh::{Endpoint, EndpointAddr};

pub use code::{Code, InvalidCode};

/// The protocol both computers speak, so nothing else talks to the sender by mistake.
const ALPN: &[u8] = b"postden/profile/1";
/// The biggest profile received: its signatures' images make it big, not its accounts.
const MAX_PROFILE: usize = 64 << 20;
/// How long the sender waits to be reachable before giving up.
const GOING_ONLINE: Duration = Duration::from_secs(30);
/// How long the receiver tries to reach the sender.
const DIALING: Duration = Duration::from_secs(60);
/// How long a computer that connected to the sender gets to show the code. Downloading has no
/// deadline, as it takes as long as the connection is slow: a computer that stops answering is
/// dropped by QUIC's idle timeout instead.
const ANSWERING: Duration = Duration::from_secs(30);

/// How many computers the sender answers at once. More wait for one to be done: a computer
/// that never shows the code gives up its place after [`ANSWERING`].
const AT_ONCE: usize = 16;

/// Closing codes: what one computer tells the other when it hangs up.
const RECEIVED: u32 = 0;
const WRONG_CODE: u32 = 1;
const GAVE_UP: u32 = 2;

/// Why sending or receiving a profile failed, phrased for the user.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("couldn't start networking: {0}")]
    Network(String),
    #[error("couldn't get online: is this computer connected to the internet?")]
    Offline,
    #[error("couldn't reach the other computer: check the code, and that it's still showing it")]
    Unreachable,
    #[error("the other computer didn't take the code")]
    WrongCode,
    #[error("the profile is bigger than {} MB", MAX_PROFILE >> 20)]
    TooBig,
    #[error("the connection broke off: {0}")]
    Broken(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A profile the sending computer is offering, until a computer with its code takes it.
#[derive(Debug)]
pub struct Offer {
    endpoint: Endpoint,
    code: Code,
    profile: Vec<u8>,
}

impl Offer {
    /// Goes online to offer `profile`, a passphrase-locked profile file, under a new code.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile is bigger than a receiver takes, networking can't
    /// start, or this computer can't get online.
    pub async fn new(profile: Vec<u8>) -> Result<Self> {
        // Refused here, not after the other computer typed the code and downloaded it all.
        if profile.len() > MAX_PROFILE {
            return Err(Error::TooBig);
        }
        let code = Code::generate().map_err(|e| Error::Network(e.to_string()))?;
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(code.secret_key())
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        // Until it's online, the sender isn't published under its key: no one could find it.
        if tokio::time::timeout(GOING_ONLINE, endpoint.online())
            .await
            .is_err()
        {
            endpoint.close().await;
            return Err(Error::Offline);
        }
        Ok(Self::on(endpoint, code, profile))
    }

    const fn on(endpoint: Endpoint, code: Code, profile: Vec<u8>) -> Self {
        Self {
            endpoint,
            code,
            profile,
        }
    }

    /// The code the receiving computer types.
    #[must_use]
    pub const fn code(&self) -> &Code {
        &self.code
    }

    /// Waits for a computer with the code, hands it the profile, and goes offline once it
    /// confirmed it has it whole. A computer without the code, or that breaks off, is hung up
    /// on. Each computer connecting is answered on its own, so one that never shows a code
    /// doesn't hold up the one that has it, up to [`AT_ONCE`] at a time. Dropping the future
    /// stops offering.
    ///
    /// # Errors
    ///
    /// Returns an error when this computer stops accepting connections.
    pub async fn deliver(self) -> Result<()> {
        let token = self.code.token();
        let profile: Arc<[u8]> = self.profile.into();
        // Dropped on return, which stops answering every other computer.
        let mut answering = tokio::task::JoinSet::new();
        let delivered = loop {
            tokio::select! {
                incoming = self.endpoint.accept(), if answering.len() < AT_ONCE => {
                    let Some(incoming) = incoming else {
                        break Err(Error::Broken("stopped accepting connections".into()));
                    };
                    answering.spawn(hand_over(incoming, token, Arc::clone(&profile)));
                }
                // Every answer is taken, so a failed one doesn't leave the next unseen.
                Some(answered) = answering.join_next() => {
                    if matches!(answered, Ok(Ok(()))) {
                        break Ok(());
                    }
                }
            }
        };
        answering.abort_all();
        self.endpoint.close().await;
        delivered
    }
}

/// Hands `profile` to the computer connecting, if it shows `token`, and waits for it to say it
/// received it.
async fn hand_over(incoming: Incoming, token: [u8; 32], profile: Arc<[u8]>) -> Result<()> {
    let connection = incoming.await.map_err(|e| Error::Broken(e.to_string()))?;
    let answered = tokio::time::timeout(ANSWERING, async {
        let (send, mut recv) = connection.accept_bi().await.map_err(broken)?;
        let shown = recv.read_to_end(token.len()).await.map_err(read_failed)?;
        if same(&shown, &token) {
            Ok(send)
        } else {
            Err(Error::WrongCode)
        }
    })
    .await
    .unwrap_or(Err(Error::Unreachable));
    let sent = match answered {
        Ok(mut send) => {
            async {
                send.write_all(&profile).await.map_err(write_failed)?;
                send.finish().map_err(|e| Error::Broken(e.to_string()))
            }
            .await
        }
        Err(e) => Err(e),
    };
    if let Err(e) = sent {
        let (code, reason) = match e {
            Error::WrongCode => (WRONG_CODE, &b"wrong code"[..]),
            _ => (GAVE_UP, &b"gave up"[..]),
        };
        connection.close(VarInt::from_u32(code), reason);
        return Err(e);
    }
    // The receiver hangs up with RECEIVED once it read the profile to its end.
    match connection.closed().await {
        ConnectionError::ApplicationClosed(close)
            if close.error_code == VarInt::from_u32(RECEIVED) =>
        {
            Ok(())
        }
        e => Err(broken(e)),
    }
}

/// Whether `a` and `b` are the same, taking as long whichever byte differs.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// Dials the computer offering a profile under `code` and downloads the profile.
///
/// # Errors
///
/// Returns an error when networking can't start, the sender can't be reached or refuses the
/// code, or the connection breaks off.
pub async fn receive(code: &Code) -> Result<Vec<u8>> {
    let endpoint = Endpoint::bind(presets::N0)
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    let received = receive_from(&endpoint, code.endpoint(), code).await;
    endpoint.close().await;
    received
}

async fn receive_from(
    endpoint: &Endpoint,
    sender: impl Into<EndpointAddr>,
    code: &Code,
) -> Result<Vec<u8>> {
    let connection = tokio::time::timeout(DIALING, endpoint.connect(sender, ALPN))
        .await
        .map_err(|_elapsed| Error::Unreachable)?
        .map_err(|_unreachable| Error::Unreachable)?;
    let received = async {
        let (mut send, mut recv) = connection.open_bi().await.map_err(broken)?;
        send.write_all(&code.token()).await.map_err(write_failed)?;
        send.finish().map_err(|e| Error::Broken(e.to_string()))?;
        recv.read_to_end(MAX_PROFILE).await.map_err(read_failed)
    }
    .await;
    if received.is_ok() {
        connection.close(VarInt::from_u32(RECEIVED), b"received");
    }
    received
}

fn broken(e: ConnectionError) -> Error {
    match e {
        ConnectionError::ApplicationClosed(close)
            if close.error_code == VarInt::from_u32(WRONG_CODE) =>
        {
            Error::WrongCode
        }
        e => Error::Broken(e.to_string()),
    }
}

fn read_failed(e: ReadToEndError) -> Error {
    match e {
        ReadToEndError::TooLong => Error::TooBig,
        ReadToEndError::Read(ReadError::ConnectionLost(e)) => broken(e),
        ReadToEndError::Read(e) => Error::Broken(e.to_string()),
    }
}

fn write_failed(e: WriteError) -> Error {
    match e {
        WriteError::ConnectionLost(e) => broken(e),
        e => Error::Broken(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use iroh::RelayMode;

    use super::*;

    /// An endpoint on this computer only: no relay, and found by its address, not its key.
    async fn local(code: Option<&Code>) -> Endpoint {
        let mut builder = Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled);
        if let Some(code) = code {
            builder = builder
                .secret_key(code.secret_key())
                .alpns(vec![ALPN.to_vec()]);
        }
        builder.bind().await.unwrap()
    }

    #[tokio::test]
    async fn the_computer_with_the_code_gets_the_profile() {
        let code = Code::generate().unwrap();
        let sender = local(Some(&code)).await;
        let address = sender.addr();
        let profile: Vec<u8> = (0..200_000_u32).map(|i| i.to_le_bytes()[0]).collect();
        let offer = tokio::spawn(Offer::on(sender, code.clone(), profile.clone()).deliver());

        let other = local(None).await;
        let received = receive_from(&other, address, &code).await.unwrap();
        other.close().await;
        assert_eq!(received, profile, "the profile arrives whole");
        offer.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_computer_that_never_shows_a_code_doesnt_hold_up_the_one_with_it() {
        let code = Code::generate().unwrap();
        let sender = local(Some(&code)).await;
        let address = sender.addr();
        let offer = tokio::spawn(Offer::on(sender, code.clone(), b"profile".to_vec()).deliver());

        let stranger = local(None).await;
        let stalled = stranger.connect(address.clone(), ALPN).await.unwrap();
        let (_send, _recv) = stalled.open_bi().await.unwrap();

        let other = local(None).await;
        let received = tokio::time::timeout(ANSWERING / 2, receive_from(&other, address, &code))
            .await
            .expect("received while the stranger still holds its connection")
            .unwrap();
        other.close().await;
        assert_eq!(received, b"profile");
        offer.await.unwrap().unwrap();
        stranger.close().await;
    }

    #[tokio::test]
    async fn computers_that_hang_up_free_their_place() {
        let code = Code::generate().unwrap();
        let sender = local(Some(&code)).await;
        let address = sender.addr();
        let offer = tokio::spawn(Offer::on(sender, code.clone(), b"profile".to_vec()).deliver());

        // As many strangers as the sender answers at once, which then hang up.
        let mut strangers = Vec::new();
        for _ in 0..AT_ONCE {
            let stranger = local(None).await;
            let connection = stranger.connect(address.clone(), ALPN).await.unwrap();
            let (mut send, _recv) = connection.open_bi().await.unwrap();
            send.write_all(b"?").await.unwrap();
            strangers.push((stranger, connection));
        }
        for (stranger, connection) in strangers {
            connection.close(VarInt::from_u32(GAVE_UP), b"");
            stranger.close().await;
        }

        let other = local(None).await;
        let received = tokio::time::timeout(ANSWERING / 2, receive_from(&other, address, &code))
            .await
            .expect("a place was free again")
            .unwrap();
        other.close().await;
        assert_eq!(received, b"profile");
        offer.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_profile_too_big_to_receive_is_never_offered() {
        let offered = Offer::new(vec![0; MAX_PROFILE + 1]).await;
        assert!(matches!(offered, Err(Error::TooBig)), "got {offered:?}");
    }

    #[tokio::test]
    async fn a_computer_without_the_code_is_hung_up_on() {
        let code = Code::generate().unwrap();
        let sender = local(Some(&code)).await;
        let address = sender.addr();
        let offer = tokio::spawn(Offer::on(sender, code.clone(), b"profile".to_vec()).deliver());

        let stranger = local(None).await;
        let wrong = Code::generate().unwrap();
        let refused = receive_from(&stranger, address.clone(), &wrong).await;
        assert!(matches!(refused, Err(Error::WrongCode)), "got {refused:?}");
        assert!(!offer.is_finished(), "still offering to the right computer");

        let received = receive_from(&stranger, address, &code).await.unwrap();
        stranger.close().await;
        assert_eq!(received, b"profile");
        offer.await.unwrap().unwrap();
    }
}
