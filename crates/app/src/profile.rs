//! Moving Postden to another computer: every account, with its signature and what it logs in
//! with, and the app's settings, in one file locked with a passphrase.
//!
//! The file is [age](https://age-encryption.org) encrypted with the passphrase (scrypt), so
//! `age --decrypt` opens it too. Inside is JSON, versioned by [`VERSION`]. Mail isn't in it: the
//! servers hold that, and the other computer fetches it again. What sorting mail learned can be
//! in it, if asked for: it tells much of the mail, as anyone can tell which words its features
//! are of.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use postden_core::account::{Account, Auth, Security, Server, Signature};
use postden_core::message::InlineImage;
use postden_core::secrets::{ExposeSecret, SecretError, SecretKey, SecretStore, SecretString};
use postden_store::{Sorting, SortingClass, SortingTaught, SortingWeight, Store};
use secrecy::SecretSlice;
use serde::{Deserialize, Serialize};

use crate::accounts::{oauth_key, password_key, smtp_password_key};
use crate::settings::{PREFETCH_COUNTS, Settings, Theme};
use crate::{Error, Result, signature};

/// The version of the profile format this Postden writes. It reads this one and older.
const VERSION: u32 = 1;

/// The shortest passphrase a profile is locked with.
pub const MIN_PASSPHRASE: usize = 8;

/// What a profile file starts with, and what [`Profile::open`] checks for.
const FORMAT: &str = "postden-profile";

/// Every account, with its signature and what it logs in with, and the app's settings: what
/// moves Postden to another computer.
#[derive(Debug, Serialize, Deserialize)]
pub struct Profile {
    format: String,
    version: u32,
    pub accounts: Vec<ProfileAccount>,
    pub settings: ProfileSettings,
    /// What sorting mail learned, if it was exported with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sorting: Option<ProfileSorting>,
}

/// What sorting mail learned, in a [`Profile`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileSorting {
    /// The version of what messages were judged by, as [`postden_ai::FEATURES`] counts: learned
    /// by others, it's no use.
    pub features: u32,
    pub classes: Vec<ProfileClass>,
    pub taught: Vec<ProfileTaught>,
}

/// One class of an account's model of sorting mail.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileClass {
    pub account: String,
    pub model: String,
    pub name: String,
    pub messages: u32,
    pub guessed: u32,
    pub correct: u32,
    /// Its weights, as base64: each a feature, then its z and n, little-endian.
    #[serde(with = "base64_bytes")]
    pub weights: Vec<u8>,
}

/// What a message was taught as, by its Message-ID.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileTaught {
    pub account: String,
    pub model: String,
    pub message_id: String,
    pub class: String,
}

/// How many bytes each weight takes in a [`ProfileClass`].
const WEIGHT_BYTES: usize = 16;

/// The `N` bytes of `chunk` from `at`.
fn bytes_at<const N: usize>(chunk: &[u8; WEIGHT_BYTES], at: usize) -> [u8; N] {
    chunk
        .get(at..at + N)
        .and_then(|b| b.try_into().ok())
        .unwrap_or([0; N])
}

impl ProfileSorting {
    fn new(features: u32, sorting: Sorting) -> Self {
        let classes = sorting
            .classes
            .into_iter()
            .map(|c| ProfileClass {
                weights: c
                    .weights
                    .iter()
                    .flat_map(|w| {
                        let mut bytes = [0; WEIGHT_BYTES];
                        let (feature, rest) = bytes.split_at_mut(8);
                        let (z, n) = rest.split_at_mut(4);
                        feature.copy_from_slice(&w.feature.to_le_bytes());
                        z.copy_from_slice(&w.z.to_le_bytes());
                        n.copy_from_slice(&w.n.to_le_bytes());
                        bytes
                    })
                    .collect(),
                account: c.account,
                model: c.model,
                name: c.name,
                messages: c.messages,
                guessed: c.guessed,
                correct: c.correct,
            })
            .collect();
        let taught = sorting
            .taught
            .into_iter()
            .map(|t| ProfileTaught {
                account: t.account,
                model: t.model,
                message_id: t.message_id,
                class: t.class,
            })
            .collect();
        Self {
            features,
            classes,
            taught,
        }
    }

    /// What was learned, as kept.
    fn sorting(&self) -> Result<Sorting> {
        let classes = self
            .classes
            .iter()
            .map(|c| {
                let (chunks, rest) = c.weights.as_chunks::<WEIGHT_BYTES>();
                if !rest.is_empty() {
                    return Err(Error::Invalid(format!(
                        "what sorting learned of {} is cut short",
                        c.name
                    )));
                }
                let weights = chunks
                    .iter()
                    .map(|chunk| SortingWeight {
                        feature: u64::from_le_bytes(bytes_at(chunk, 0)),
                        z: f32::from_le_bytes(bytes_at(chunk, 8)),
                        n: f32::from_le_bytes(bytes_at(chunk, 12)),
                    })
                    .collect();
                Ok(SortingClass {
                    account: c.account.clone(),
                    model: c.model.clone(),
                    name: c.name.clone(),
                    messages: c.messages,
                    guessed: c.guessed,
                    correct: c.correct,
                    weights,
                })
            })
            .collect::<Result<_>>()?;
        let taught = self
            .taught
            .iter()
            .map(|t| SortingTaught {
                account: t.account.clone(),
                model: t.model.clone(),
                message_id: t.message_id.clone(),
                class: t.class.clone(),
            })
            .collect();
        Ok(Sorting { classes, taught })
    }
}

/// One account in a [`Profile`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileAccount {
    pub id: String,
    pub display_name: Option<String>,
    pub label: Option<String>,
    pub auth: String,
    pub imap: ProfileServer,
    pub smtp: ProfileServer,
    pub signature: Option<ProfileSignature>,
    pub secrets: ProfileSecrets,
}

impl ProfileAccount {
    /// What to call the account: its label, or else its display name, or else its address.
    #[must_use]
    pub fn name(&self) -> &str {
        self.label
            .as_deref()
            .or(self.display_name.as_deref())
            .unwrap_or(&self.id)
    }

    fn new(account: Account, signature: Option<&Signature>, secrets: ProfileSecrets) -> Self {
        Self {
            id: account.id,
            display_name: account.display_name,
            label: account.label,
            auth: account.auth.as_str().to_owned(),
            imap: ProfileServer::from(account.imap),
            smtp: ProfileServer::from(account.smtp),
            signature: signature.map(ProfileSignature::from),
            secrets,
        }
    }

    /// The account as Postden keeps it.
    fn account(&self) -> Result<Account> {
        let auth = self
            .auth
            .parse()
            .map_err(|e: postden_core::account::UnknownAuth| Error::Invalid(e.to_string()))?;
        Ok(Account {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            label: self.label.clone(),
            imap: self.imap.server()?,
            smtp: self.smtp.server()?,
            auth,
        })
    }
}

/// A mail server in a [`ProfileAccount`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileServer {
    pub host: String,
    pub port: u16,
    pub security: String,
    pub username: String,
}

/// An account's signature in a [`ProfileAccount`]. Its images' bytes are base64.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileSignature {
    pub html: String,
    pub source: bool,
    pub images: Vec<ProfileImage>,
}

/// One of a signature's images.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileImage {
    pub cid: String,
    pub name: String,
    pub mime: String,
    #[serde(with = "base64_bytes")]
    pub data: Vec<u8>,
}

/// What an account logs in with: its passwords, or its Google sign-in.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProfileSecrets {
    #[serde(with = "secret", default, skip_serializing_if = "Option::is_none")]
    pub password: Option<SecretString>,
    #[serde(with = "secret", default, skip_serializing_if = "Option::is_none")]
    pub smtp_password: Option<SecretString>,
    #[serde(with = "secret", default, skip_serializing_if = "Option::is_none")]
    pub oauth_refresh_token: Option<SecretString>,
}

/// The app's settings in a [`Profile`]: all but those about this computer's copy of Postden,
/// such as a skipped update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSettings {
    pub theme: Theme,
    pub poll_interval_secs: u64,
    /// How many of the inbox's newest messages are downloaded ahead of their opening. Absent
    /// from profiles exported before Postden had it, which keep this computer's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefetch: Option<usize>,
    /// Whether opened messages stay downloaded. Absent from profiles exported before Postden had
    /// it, which keep this computer's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_opened: Option<bool>,
    /// Whether the people mail is sent to are remembered. Absent from profiles exported before
    /// Postden had it, which keep this computer's. The contacts themselves stay on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember_contacts: Option<bool>,
    /// Whether inbox mail is sorted. Absent from profiles exported before Postden sorted mail,
    /// which keep this computer's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_mail: Option<bool>,
    pub notify_new_mail: bool,
    pub keep_running: bool,
    pub check_updates: bool,
}

/// What [`Profile::import`] does with what's already set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportOptions {
    /// Replace accounts set up already with the profile's; otherwise they're kept as they are.
    pub replace_accounts: bool,
    /// Use the profile's settings; otherwise they're kept as they are.
    pub settings: bool,
    /// Use what the profile's sorting learned, if it has it, in place of what was learned here
    /// of its accounts.
    pub sorting: bool,
}

/// What [`Profile::import`] did, by account address.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Imported {
    pub added: Vec<String>,
    pub replaced: Vec<String>,
    /// Set up already, and kept as they were.
    pub kept: Vec<String>,
    /// Imported, but their passwords weren't saved, as they come from a password command.
    pub without_secrets: Vec<String>,
    /// Not imported, and why. The others were.
    pub failed: Vec<(String, String)>,
    /// Whether what sorting learned was imported, or why it wasn't, if it was to be.
    pub sorting: Option<Result<(), String>>,
}

impl Profile {
    /// Gathers every account in `store`, with its signature and what it logs in with from
    /// `secrets`, and the app's settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the store or the secrets can't be read.
    pub fn collect(store: &Store, secrets: &dyn SecretStore) -> Result<Self> {
        let accounts = store
            .accounts()?
            .into_iter()
            .map(|account| {
                let id = account.id.clone();
                // Only what the account logs in with: a password command may answer for any key.
                let secrets = match account.auth {
                    Auth::Password => ProfileSecrets {
                        password: secrets.get(&password_key(&id))?,
                        smtp_password: smtp_password(secrets, &id)?,
                        oauth_refresh_token: None,
                    },
                    Auth::Google => ProfileSecrets {
                        oauth_refresh_token: secrets.get(&oauth_key(&id))?,
                        ..ProfileSecrets::default()
                    },
                };
                Ok(ProfileAccount::new(
                    account,
                    store.signature(&id)?.as_ref(),
                    secrets,
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            accounts,
            settings: ProfileSettings::from(&Settings::load(store)?),
            sorting: None,
        })
    }

    /// The profile, with what sorting mail learned in `store` too.
    ///
    /// # Errors
    ///
    /// Returns an error when the store can't be read.
    pub fn with_sorting(mut self, store: &Store) -> Result<Self> {
        let (features, sorting) = store.sorting_learned()?;
        // Never said, nothing was learned yet.
        let features = features.unwrap_or(postden_ai::FEATURES);
        self.sorting = Some(ProfileSorting::new(features, sorting));
        Ok(self)
    }

    /// The profile as a file, encrypted with `passphrase`.
    ///
    /// # Errors
    ///
    /// Returns an error when the passphrase is shorter than [`MIN_PASSPHRASE`], or the
    /// encryption fails.
    pub fn seal(&self, passphrase: &SecretString) -> Result<Vec<u8>> {
        self.seal_with(passphrase, None)
    }

    /// Seals the profile with `passphrase` and writes it to `path`, replacing what's there only
    /// once it's whole: a profile there already survives a failed export.
    ///
    /// # Errors
    ///
    /// As [`Profile::seal`], or when the file can't be written.
    pub fn save(&self, path: &Path, passphrase: &SecretString) -> Result<()> {
        replace_file(path, &self.seal(passphrase)?)?;
        Ok(())
    }

    /// [`Profile::seal`], with scrypt's work factor set, as tests do to stay quick. `None` lets
    /// age pick one that takes about a second here.
    fn seal_with(&self, passphrase: &SecretString, log_n: Option<u8>) -> Result<Vec<u8>> {
        if passphrase.expose_secret().chars().count() < MIN_PASSPHRASE {
            return Err(Error::Invalid(format!(
                "the passphrase needs at least {MIN_PASSPHRASE} characters"
            )));
        }
        let json =
            SecretSlice::from(serde_json::to_vec(self).map_err(|e| Error::Invalid(e.to_string()))?);
        let mut recipient = age::scrypt::Recipient::new(passphrase.clone());
        if let Some(log_n) = log_n {
            recipient.set_work_factor(log_n);
        }
        age::encrypt(&recipient, json.expose_secret())
            .map_err(|e| Error::Invalid(format!("couldn't encrypt the profile: {e}")))
    }

    /// Reads a profile from a file [`Profile::seal`] wrote, with the passphrase it was sealed
    /// with.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongPassphrase`] for another passphrase, or an error when the file isn't
    /// a profile, or one a newer Postden wrote.
    pub fn open(file: &[u8], passphrase: &SecretString) -> Result<Self> {
        if !is_sealed(file) {
            return Err(Error::Invalid("this isn't a Postden profile".into()));
        }
        let identity = age::scrypt::Identity::new(passphrase.clone());
        let json = SecretSlice::from(age::decrypt(&identity, file).map_err(|e| match e {
            age::DecryptError::DecryptionFailed
            | age::DecryptError::KeyDecryptionFailed
            | age::DecryptError::NoMatchingKeys => Error::WrongPassphrase,
            e => Error::Invalid(format!("couldn't decrypt the profile: {e}")),
        })?);
        let profile: Self = serde_json::from_slice(json.expose_secret())
            .map_err(|e| Error::Invalid(format!("this isn't a Postden profile: {e}")))?;
        if profile.format != FORMAT {
            return Err(Error::Invalid("this isn't a Postden profile".into()));
        }
        if profile.version > VERSION {
            return Err(Error::Invalid(
                "a newer Postden made this profile; update Postden to import it".into(),
            ));
        }
        for account in &profile.accounts {
            account.account()?;
        }
        Ok(profile)
    }

    /// Which of the profile's accounts `store` has set up already, by address.
    ///
    /// # Errors
    ///
    /// Returns an error when the store can't be read.
    pub fn existing(&self, store: &Store) -> Result<HashSet<String>> {
        let ids: HashSet<String> = store.accounts()?.into_iter().map(|a| a.id).collect();
        Ok(self
            .accounts
            .iter()
            .filter(|a| ids.contains(&a.id))
            .map(|a| a.id.clone())
            .collect())
    }

    /// Sets up the profile's accounts in `store`, their passwords or sign-ins in `secrets`, and,
    /// if `options` say so, its settings. An account set up already is replaced or kept, as
    /// `options` say; a replaced one keeps its cached mail.
    ///
    /// An account that can't be saved is listed in [`Imported::failed`], and the others are
    /// imported all the same, so what's returned is always what changed.
    ///
    /// # Errors
    ///
    /// Returns an error, having changed nothing, when the store can't be read, the settings can't
    /// be saved, or an account signs in with Google while `secrets` is read-only, as the sign-in
    /// couldn't be kept.
    pub fn import(
        &self,
        store: &mut Store,
        secrets: &dyn SecretStore,
        options: ImportOptions,
    ) -> Result<Imported> {
        let existing = self.existing(store)?;
        let mut imported = Imported::default();
        let mut taken = Vec::new();
        for profile in &self.accounts {
            let exists = existing.contains(&profile.id);
            if exists && !options.replace_accounts {
                imported.kept.push(profile.id.clone());
            } else {
                taken.push((profile, profile.account()?, exists));
            }
        }
        if secrets.is_read_only()
            && let Some((google, ..)) = taken.iter().find(|(_, a, _)| a.auth == Auth::Google)
        {
            return Err(Error::Invalid(format!(
                "{} signs in with Google, which needs the OS keyring to keep the sign-in, but \
                 passwords come from a command (POSTDEN_PASSWORD_COMMAND)",
                google.id
            )));
        }
        // Before the accounts, so failing to save them changes nothing.
        if options.settings {
            let mut settings = Settings::load(store)?;
            self.settings.apply(&mut settings);
            settings.save(store)?;
        }
        for (profile, account, exists) in taken {
            let id = account.id.clone();
            match import_account(store, secrets, profile, &account) {
                Ok(true) if exists => imported.replaced.push(id),
                Ok(true) => imported.added.push(id),
                Ok(false) => {
                    imported.without_secrets.push(id.clone());
                    if exists {
                        imported.replaced.push(id);
                    } else {
                        imported.added.push(id);
                    }
                }
                Err(e) => imported.failed.push((id, e.to_string())),
            }
        }
        if options.sorting
            && let Some(sorting) = &self.sorting
        {
            imported.sorting = Some(self.import_sorting(store, sorting));
        }
        Ok(imported)
    }

    /// Replaces what sorting learned here of the profile's accounts set up with what it has.
    fn import_sorting(
        &self,
        store: &mut Store,
        sorting: &ProfileSorting,
    ) -> std::result::Result<(), String> {
        if sorting.features != postden_ai::FEATURES {
            return Err("it was learned by another version of Postden".into());
        }
        // Only of the same mailboxes: an account kept here, or not replaced, may read others.
        let set_up: HashMap<String, Server> = store
            .accounts()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|a| (a.id, a.imap))
            .collect();
        let accounts: Vec<&str> = self
            .accounts
            .iter()
            .filter(|a| {
                a.account().is_ok_and(|account| {
                    set_up
                        .get(&a.id)
                        .is_some_and(|here| same_mailboxes(here, &account.imap))
                })
            })
            .map(|a| a.id.as_str())
            .collect();
        if accounts.is_empty() {
            return Err("none of its accounts reads the same mail here".into());
        }
        let sorting = sorting.sorting().map_err(|e| e.to_string())?;
        store
            .replace_sorting(&accounts, &sorting, postden_ai::FEATURES)
            .map_err(|e| e.to_string())
    }
}

/// Whether `a` and `b` log in to the same mailboxes: host names are the same in any case.
fn same_mailboxes(a: &Server, b: &Server) -> bool {
    a.host.eq_ignore_ascii_case(&b.host)
        && (a.port, a.security, &a.username) == (b.port, b.security, &b.username)
}

/// Saves one of a profile's accounts: its secrets, then the account, its names and its
/// signature at once. Returns `false` when its secrets weren't saved, as `secrets` is read-only.
///
/// When it fails, it changes nothing: the store is written all at once, and the secrets it
/// had before are put back.
fn import_account(
    store: &mut Store,
    secrets: &dyn SecretStore,
    profile: &ProfileAccount,
    account: &Account,
) -> Result<bool> {
    let id = &account.id;
    let given = &profile.secrets;
    let wanted = [
        (password_key(id), given.password.clone()),
        (smtp_password_key(id), given.smtp_password.clone()),
        (oauth_key(id), given.oauth_refresh_token.clone()),
    ];
    // A password command can't be written, so there's nothing to put back.
    let before = if secrets.is_read_only() {
        None
    } else {
        let read = wanted
            .iter()
            .map(|(key, _)| Ok((key.clone(), secrets.get(key)?)));
        Some(read.collect::<Result<Vec<_>>>()?)
    };
    // Secrets first, as when an account is added: it's never listed without them.
    let imported = write_secrets(secrets, &wanted)
        .map_err(Error::from)
        .and_then(|saved| {
            let signature = profile
                .signature
                .as_ref()
                .map(ProfileSignature::signature)
                .filter(|s| !signature::is_blank(s));
            store.replace_account(account, signature.as_ref())?;
            Ok(saved)
        });
    match (imported, before) {
        (Err(e), Some(before)) => match write_secrets(secrets, &before) {
            Ok(_) => Err(e),
            Err(undo) => Err(Error::Invalid(format!(
                "{e}; and its previous passwords couldn't be put back: {undo}"
            ))),
        },
        (imported, _) => imported,
    }
}

/// The account's own SMTP password, if it has one. A password command that fails for it
/// may just not know the key, as when logging in; any other store's failure is an error, so
/// an export never leaves it out unnoticed.
fn smtp_password(secrets: &dyn SecretStore, id: &str) -> Result<Option<SecretString>> {
    match secrets.get(&smtp_password_key(id)) {
        Err(_) if secrets.is_read_only() => Ok(None),
        read => Ok(read?),
    }
}

/// Writes `bytes` to a new file beside `path`, then renames it over `path`, so `path` holds
/// either what it held or all of `bytes`, never part of them.
fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let (partial, mut file) = create_partial(path)?;
    let replaced = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&partial, path));
    if replaced.is_err() {
        // Only ever the file made here: `create_partial` never opens one that was there.
        let _ignored = std::fs::remove_file(&partial);
    }
    replaced
}

/// Makes a new, empty file beside `path` to write it in, readable only by this user. A file or
/// link already at a name tried is left alone, never followed nor emptied, and the next name
/// tried: anyone who can write in the folder could have put it there.
fn create_partial(path: &Path) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    const ATTEMPTS: u32 = 100;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let pid = std::process::id();
    for attempt in 0..ATTEMPTS {
        let partial = path.with_file_name(format!(".{name}.{pid}.{attempt}.partial"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&partial) {
            Ok(file) => return Ok((partial, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "couldn't find a free name beside the profile to write it in",
    ))
}

/// Whether `file` is age-encrypted, as a sealed profile is.
#[must_use]
pub fn is_sealed(file: &[u8]) -> bool {
    file.starts_with(b"age-encryption.org/v1\n")
}

/// Sets each key to its secret, or deletes it for `None`, so a replaced account doesn't keep
/// an old password. Returns `false`, having written nothing, when `secrets` is read-only.
pub(crate) fn write_secrets(
    secrets: &dyn SecretStore,
    writes: &[(SecretKey, Option<SecretString>)],
) -> Result<bool, SecretError> {
    for (key, secret) in writes {
        let written = secret
            .as_ref()
            .map_or_else(|| secrets.delete(key), |secret| secrets.set(key, secret));
        match written {
            Ok(()) => {}
            Err(SecretError::ReadOnly) => return Ok(false),
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

impl From<Server> for ProfileServer {
    fn from(server: Server) -> Self {
        Self {
            host: server.host,
            port: server.port,
            security: server.security.as_str().to_owned(),
            username: server.username,
        }
    }
}

impl ProfileServer {
    fn server(&self) -> Result<Server> {
        let security: Security = self
            .security
            .parse()
            .map_err(|e: postden_core::account::UnknownSecurity| Error::Invalid(e.to_string()))?;
        Ok(Server {
            host: self.host.clone(),
            port: self.port,
            security,
            username: self.username.clone(),
        })
    }
}

impl From<&Signature> for ProfileSignature {
    fn from(signature: &Signature) -> Self {
        Self {
            html: signature.html.clone(),
            source: signature.source,
            images: signature
                .images
                .iter()
                .map(|image| ProfileImage {
                    cid: image.cid.clone(),
                    name: image.name.clone(),
                    mime: image.mime.clone(),
                    data: image.data.to_vec(),
                })
                .collect(),
        }
    }
}

impl ProfileSignature {
    fn signature(&self) -> Signature {
        Signature {
            html: self.html.clone(),
            source: self.source,
            images: self
                .images
                .iter()
                .map(|image| InlineImage {
                    cid: image.cid.clone(),
                    name: image.name.clone(),
                    mime: image.mime.clone(),
                    data: image.data.as_slice().into(),
                })
                .collect(),
        }
    }
}

impl From<&Settings> for ProfileSettings {
    fn from(settings: &Settings) -> Self {
        Self {
            theme: settings.theme,
            poll_interval_secs: settings.poll_interval.as_secs(),
            prefetch: Some(settings.prefetch),
            keep_opened: Some(settings.keep_opened),
            remember_contacts: Some(settings.remember_contacts),
            sort_mail: Some(settings.sort_mail),
            notify_new_mail: settings.notify_new_mail,
            keep_running: settings.keep_running,
            check_updates: settings.check_updates,
        }
    }
}

impl ProfileSettings {
    /// Sets `settings` to these, but for what's about this computer's copy of Postden.
    fn apply(self, settings: &mut Settings) {
        settings.theme = self.theme;
        if self.poll_interval_secs > 0 {
            settings.poll_interval = Duration::from_secs(self.poll_interval_secs);
        }
        if let Some(prefetch) = self.prefetch.filter(|n| PREFETCH_COUNTS.contains(n)) {
            settings.prefetch = prefetch;
        }
        if let Some(keep_opened) = self.keep_opened {
            settings.keep_opened = keep_opened;
        }
        if let Some(remember_contacts) = self.remember_contacts {
            settings.remember_contacts = remember_contacts;
        }
        if let Some(sort_mail) = self.sort_mail {
            settings.sort_mail = sort_mail;
        }
        settings.notify_new_mail = self.notify_new_mail;
        settings.keep_running = self.keep_running;
        settings.check_updates = self.check_updates;
    }
}

/// A secret as a JSON string.
mod secret {
    use postden_core::secrets::{ExposeSecret as _, SecretString};
    use serde::{Deserialize as _, Deserializer, Serializer};

    #[expect(
        clippy::ref_option,
        reason = "serde's `with` hands the field by reference"
    )]
    pub(super) fn serialize<S: Serializer>(
        secret: &Option<SecretString>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match secret {
            Some(secret) => s.serialize_some(secret.expose_secret()),
            None => s.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<SecretString>, D::Error> {
        Ok(Option::<String>::deserialize(d)?.map(SecretString::from))
    }
}

/// Bytes as a base64 JSON string.
mod base64_bytes {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use postden_core::secrets::SecretKey;

    use super::*;
    use crate::testing::{Memory, ReadOnly};

    fn server(host: &str) -> Server {
        Server {
            host: host.into(),
            port: 993,
            security: Security::Tls,
            username: "ada@example.com".into(),
        }
    }

    fn account(id: &str, auth: Auth) -> Account {
        Account {
            id: id.into(),
            display_name: Some("Ada".into()),
            label: Some("Work".into()),
            imap: server("imap.example.com"),
            smtp: Server {
                port: 587,
                security: Security::StartTls,
                ..server("smtp.example.com")
            },
            auth,
        }
    }

    fn secret(secrets: &dyn SecretStore, key: &SecretKey) -> Option<String> {
        secrets
            .get(key)
            .unwrap()
            .map(|s| s.expose_secret().to_owned())
    }

    /// A store with a password account, with a signature, and a Google one; and their secrets.
    fn set_up() -> (Store, Memory) {
        let mut store = Store::open_in_memory().unwrap();
        let secrets = Memory::default();
        store
            .save_account(&account("ada@example.com", Auth::Password))
            .unwrap();
        store
            .save_account(&account("ada@gmail.com", Auth::Google))
            .unwrap();
        let signature = Signature {
            html: r#"<p>Ada <img src="cid:logo"></p>"#.into(),
            source: true,
            images: vec![InlineImage {
                cid: "logo".into(),
                name: "logo.png".into(),
                mime: "image/png".into(),
                data: Arc::from(&b"\x89PNG\r\n\x1a\n"[..]),
            }],
        };
        store
            .save_signature("ada@example.com", Some(&signature))
            .unwrap();
        let set = |key, value: &str| secrets.set(&key, &SecretString::from(value)).unwrap();
        set(password_key("ada@example.com"), "imap-pass");
        set(smtp_password_key("ada@example.com"), "smtp-pass");
        set(oauth_key("ada@gmail.com"), "1//refresh");
        let settings = Settings {
            theme: Theme::Dark,
            poll_interval: Duration::from_mins(5),
            prefetch: 50,
            keep_opened: true,
            sort_mail: false,
            notify_new_mail: false,
            remember_contacts: true,
            ..Settings::default()
        };
        settings.save(&store).unwrap();
        (store, secrets)
    }

    fn sealed(store: &Store, secrets: &dyn SecretStore) -> Vec<u8> {
        Profile::collect(store, secrets)
            .unwrap()
            .seal_with(&SecretString::from("correct horse"), Some(2))
            .unwrap()
    }

    fn open(file: &[u8], passphrase: &str) -> Result<Profile> {
        Profile::open(file, &SecretString::from(passphrase))
    }

    const EVERYTHING: ImportOptions = ImportOptions {
        replace_accounts: true,
        settings: true,
        sorting: true,
    };

    #[test]
    fn settings_a_profile_lacks_stay_as_they_are() {
        // As exported before Postden could download ahead.
        let older: ProfileSettings = serde_json::from_str(
            r#"{"theme":"dark","poll_interval_secs":300,"notify_new_mail":false,
                "keep_running":true,"check_updates":true}"#,
        )
        .unwrap();
        let mut settings = Settings {
            prefetch: 100,
            keep_opened: true,
            remember_contacts: true,
            sort_mail: false,
            ..Settings::default()
        };
        older.apply(&mut settings);
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.prefetch, 100, "kept");
        assert!(settings.keep_opened, "kept");
        assert!(settings.remember_contacts, "kept");
        assert!(!settings.sort_mail, "kept");

        let odd = ProfileSettings {
            prefetch: Some(7),
            ..older
        };
        odd.apply(&mut settings);
        assert_eq!(settings.prefetch, 100, "not a choice Postden offers");
    }

    #[test]
    fn a_profile_moves_accounts_signatures_secrets_and_settings() {
        let (store, secrets) = set_up();
        let file = sealed(&store, &secrets);
        assert!(is_sealed(&file), "it's encrypted");
        assert!(
            !file.windows(9).any(|w| w == b"imap-pass"),
            "no secret shows in the file"
        );

        let (mut there, there_secrets) = (Store::open_in_memory().unwrap(), Memory::default());
        let profile = open(&file, "correct horse").unwrap();
        assert!(profile.existing(&there).unwrap().is_empty());
        let imported = profile
            .import(&mut there, &there_secrets, EVERYTHING)
            .unwrap();
        assert_eq!(imported.added, ["ada@example.com", "ada@gmail.com"]);

        assert_eq!(there.accounts().unwrap(), store.accounts().unwrap());
        assert_eq!(
            there.signature("ada@example.com").unwrap(),
            store.signature("ada@example.com").unwrap()
        );
        assert_eq!(there.signature("ada@gmail.com").unwrap(), None);
        assert_eq!(*there_secrets.0.lock().unwrap(), *secrets.0.lock().unwrap());
        let settings = Settings::load(&there).unwrap();
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.poll_interval, Duration::from_mins(5));
        assert_eq!(settings.prefetch, 50);
        assert!(settings.keep_opened);
        assert!(settings.remember_contacts);
        assert!(!settings.sort_mail);
        assert!(!settings.notify_new_mail);
    }

    /// What sorting learned of an account's mail: a class, and what one message was taught as.
    fn learned(account: &str, class: &str) -> Sorting {
        Sorting {
            classes: vec![SortingClass {
                account: account.into(),
                model: "folders".into(),
                name: class.into(),
                messages: 3,
                guessed: 2,
                correct: 1,
                weights: vec![
                    SortingWeight {
                        feature: u64::MAX,
                        z: -1.5,
                        n: 0.25,
                    },
                    SortingWeight {
                        feature: 7,
                        z: 2.0,
                        n: 4.0,
                    },
                ],
            }],
            taught: vec![SortingTaught {
                account: account.into(),
                model: "folders".into(),
                message_id: "1@example.com".into(),
                class: class.into(),
            }],
        }
    }

    #[test]
    fn what_sorting_learned_moves_only_when_asked_and_replaces_whats_there() {
        let (mut store, secrets) = set_up();
        store
            .replace_sorting(
                &["ada@example.com"],
                &learned("ada@example.com", "Receipts"),
                postden_ai::FEATURES,
            )
            .unwrap();
        let without = open(&sealed(&store, &secrets), "correct horse").unwrap();
        assert!(without.sorting.is_none(), "not unless asked for");

        let file = Profile::collect(&store, &secrets)
            .unwrap()
            .with_sorting(&store)
            .unwrap()
            .seal_with(&SecretString::from("correct horse"), Some(2))
            .unwrap();
        let (mut there, there_secrets) = (Store::open_in_memory().unwrap(), Memory::default());
        there
            .save_account(&account("ada@example.com", Auth::Password))
            .unwrap();
        there
            .replace_sorting(
                &["ada@example.com"],
                &learned("ada@example.com", "Old"),
                postden_ai::FEATURES,
            )
            .unwrap();
        let profile = open(&file, "correct horse").unwrap();
        let keep = ImportOptions {
            sorting: false,
            ..EVERYTHING
        };
        let imported = profile.import(&mut there, &there_secrets, keep).unwrap();
        assert_eq!(imported.sorting, None);
        assert_eq!(there.sorting().unwrap(), learned("ada@example.com", "Old"));

        let imported = profile
            .import(&mut there, &there_secrets, EVERYTHING)
            .unwrap();
        assert_eq!(imported.sorting, Some(Ok(())));
        assert_eq!(there.sorting().unwrap(), store.sorting().unwrap());
        // Where sorting never ran, it's kept as learned by the same features.
        let _sorted = crate::sorting::Learner::default()
            .sort(&mut there, 0)
            .unwrap();
        assert_eq!(there.sorting().unwrap(), store.sorting().unwrap());
    }

    #[test]
    fn what_another_version_learned_isnt_imported() {
        let (mut store, secrets) = set_up();
        store
            .replace_sorting(
                &["ada@example.com"],
                &learned("ada@example.com", "Receipts"),
                postden_ai::FEATURES,
            )
            .unwrap();
        let mut profile = Profile::collect(&store, &secrets)
            .unwrap()
            .with_sorting(&store)
            .unwrap();
        if let Some(sorting) = &mut profile.sorting {
            sorting.features += 1;
        }
        let mut there = Store::open_in_memory().unwrap();
        let imported = profile
            .import(&mut there, &Memory::default(), EVERYTHING)
            .unwrap();
        assert!(matches!(imported.sorting, Some(Err(_))), "{imported:?}");
        assert_eq!(there.sorting().unwrap(), Sorting::default());
    }

    #[test]
    fn what_sorting_learned_of_other_mailboxes_isnt_imported() {
        let (mut store, secrets) = set_up();
        store
            .replace_sorting(
                &["ada@example.com"],
                &learned("ada@example.com", "Receipts"),
                postden_ai::FEATURES,
            )
            .unwrap();
        store
            .replace_sorting(
                &["ada@gmail.com"],
                &learned("ada@gmail.com", "Receipts"),
                postden_ai::FEATURES,
            )
            .unwrap();
        let profile = Profile::collect(&store, &secrets)
            .unwrap()
            .with_sorting(&store)
            .unwrap();
        let mut there = Store::open_in_memory().unwrap();
        let mut elsewhere = account("ada@example.com", Auth::Password);
        elsewhere.imap.host = "imap.elsewhere.example".into();
        there.save_account(&elsewhere).unwrap();
        let mut shouting = account("ada@gmail.com", Auth::Google);
        shouting.imap.host = shouting.imap.host.to_uppercase();
        there.save_account(&shouting).unwrap();
        let kept = ImportOptions {
            replace_accounts: false,
            ..EVERYTHING
        };
        let imported = profile
            .import(&mut there, &Memory::default(), kept)
            .unwrap();
        assert_eq!(
            imported.sorting,
            Some(Ok(())),
            "Gmail's is the same in any case"
        );
        let accounts: HashSet<String> = there
            .sorting()
            .unwrap()
            .classes
            .into_iter()
            .map(|c| c.account)
            .collect();
        assert_eq!(
            accounts,
            HashSet::from(["ada@gmail.com".to_owned()]),
            "not of the account reading other mail"
        );

        let mut alone = Store::open_in_memory().unwrap();
        alone.save_account(&elsewhere).unwrap();
        let mut other_gmail = account("ada@gmail.com", Auth::Google);
        other_gmail.imap.username = "someone.else@gmail.com".into();
        alone.save_account(&other_gmail).unwrap();
        let imported = profile
            .import(&mut alone, &Memory::default(), kept)
            .unwrap();
        assert!(matches!(imported.sorting, Some(Err(_))), "{imported:?}");
        assert_eq!(alone.sorting().unwrap(), Sorting::default());
    }

    #[cfg(unix)]
    #[test]
    fn saving_never_writes_through_a_file_planted_beside_the_profile() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("postden-planted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profile.postden");
        let victim = dir.join("victim");
        std::fs::write(&victim, b"keep me").unwrap();
        let first = dir.join(format!(".profile.postden.{}.0.partial", std::process::id()));
        std::os::unix::fs::symlink(&victim, &first).unwrap();

        replace_file(&path, b"profile").unwrap();
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"keep me",
            "the link isn't followed"
        );
        assert!(std::fs::symlink_metadata(&first).is_ok(), "nor removed");
        assert_eq!(std::fs::read(&path).unwrap(), b"profile");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "only its owner reads it");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_replaces_a_profile_only_once_the_new_one_is_whole() {
        let (store, secrets) = set_up();
        let profile = Profile::collect(&store, &secrets).unwrap();
        let dir = std::env::temp_dir().join(format!("postden-profile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("backup.postden");
        std::fs::write(&path, b"the old backup").unwrap();

        assert!(profile.save(&path, &SecretString::from("short")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"the old backup", "kept");

        replace_file(&path, b"the new backup").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"the new backup");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(left.len(), 1, "no partial file is left");

        let nowhere = dir.join("missing").join("backup.postden");
        assert!(replace_file(&nowhere, b"x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_wrong_passphrase_or_another_file_is_refused() {
        let (store, secrets) = set_up();
        let file = sealed(&store, &secrets);
        assert!(matches!(
            open(&file, "wrong horse"),
            Err(Error::WrongPassphrase)
        ));
        assert!(open(b"{\"format\":\"postden-profile\"}", "correct horse").is_err());
        let short = Profile::collect(&store, &secrets)
            .unwrap()
            .seal(&SecretString::from("short"));
        assert!(short.is_err(), "too short a passphrase");
    }

    #[test]
    fn accounts_set_up_already_are_kept_unless_replaced() {
        let (store, secrets) = set_up();
        let profile = open(&sealed(&store, &secrets), "correct horse").unwrap();

        let (mut there, there_secrets) = (Store::open_in_memory().unwrap(), Memory::default());
        let mut mine = account("ada@example.com", Auth::Password);
        mine.label = Some("Mine".into());
        there.save_account(&mine).unwrap();
        there_secrets
            .set(
                &smtp_password_key("ada@example.com"),
                &SecretString::from("old"),
            )
            .unwrap();
        Settings::default().save(&there).unwrap();

        let keep = ImportOptions::default();
        let imported = profile.import(&mut there, &there_secrets, keep).unwrap();
        assert_eq!(imported.kept, ["ada@example.com"]);
        assert_eq!(imported.added, ["ada@gmail.com"]);
        let label = |s: &Store| s.account("ada@example.com").unwrap().unwrap().label;
        assert_eq!(label(&there).as_deref(), Some("Mine"));
        assert_eq!(
            Settings::load(&there).unwrap().theme,
            Theme::System,
            "settings are kept"
        );

        let imported = profile
            .import(&mut there, &there_secrets, EVERYTHING)
            .unwrap();
        assert_eq!(imported.replaced, ["ada@example.com", "ada@gmail.com"]);
        assert_eq!(label(&there).as_deref(), Some("Work"));
        assert_eq!(
            secret(&there_secrets, &smtp_password_key("ada@example.com")).as_deref(),
            Some("smtp-pass")
        );
    }

    #[test]
    fn a_replaced_account_forgets_how_it_logged_in_before() {
        let (store, secrets) = set_up();
        secrets
            .delete(&smtp_password_key("ada@example.com"))
            .unwrap();
        let profile = open(&sealed(&store, &secrets), "correct horse").unwrap();
        let (mut there, there_secrets) = (Store::open_in_memory().unwrap(), Memory::default());
        there
            .save_account(&account("ada@example.com", Auth::Google))
            .unwrap();
        there_secrets
            .set(&oauth_key("ada@example.com"), &SecretString::from("old"))
            .unwrap();
        there_secrets
            .set(
                &smtp_password_key("ada@example.com"),
                &SecretString::from("old"),
            )
            .unwrap();
        profile
            .import(&mut there, &there_secrets, EVERYTHING)
            .unwrap();
        assert_eq!(secret(&there_secrets, &oauth_key("ada@example.com")), None);
        assert_eq!(
            secret(&there_secrets, &smtp_password_key("ada@example.com")),
            None
        );
        assert_eq!(
            secret(&there_secrets, &password_key("ada@example.com")).as_deref(),
            Some("imap-pass")
        );
    }

    /// A password command that prints the same password for any key.
    #[derive(Debug)]
    struct Echo;

    impl SecretStore for Echo {
        fn get(&self, _: &SecretKey) -> Result<Option<SecretString>, SecretError> {
            Ok(Some(SecretString::from("secret")))
        }
        fn set(&self, _: &SecretKey, _: &SecretString) -> Result<(), SecretError> {
            Err(SecretError::ReadOnly)
        }
        fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
            Err(SecretError::ReadOnly)
        }
        fn is_read_only(&self) -> bool {
            true
        }
    }

    /// A keyring holding `inner`'s secrets that can't read any SMTP password when `unreadable`,
    /// and can't write the SMTP password of the account `unwritable`.
    #[derive(Debug, Default)]
    struct Failing {
        inner: Memory,
        unreadable: bool,
        unwritable: &'static str,
    }

    fn smtp(key: &SecretKey) -> bool {
        key.kind == postden_core::secrets::SecretKind::SmtpPassword
    }

    impl SecretStore for Failing {
        fn get(&self, key: &SecretKey) -> Result<Option<SecretString>, SecretError> {
            if self.unreadable && smtp(key) {
                return Err(SecretError::Unavailable("locked".into()));
            }
            self.inner.get(key)
        }
        fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<(), SecretError> {
            if key.account == self.unwritable && smtp(key) {
                return Err(SecretError::Unavailable("locked".into()));
            }
            self.inner.set(key, secret)
        }
        fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
            self.inner.delete(key)
        }
    }

    #[test]
    fn a_keyring_failing_to_read_fails_the_export() {
        let (store, secrets) = set_up();
        let failing = Failing {
            inner: secrets,
            unreadable: true,
            ..Failing::default()
        };
        let error = Profile::collect(&store, &failing).unwrap_err();
        assert!(error.to_string().contains("locked"), "got {error}");
    }

    #[test]
    fn an_account_that_fails_to_import_is_left_as_it_was() {
        let (store, secrets) = set_up();
        let profile = open(&sealed(&store, &secrets), "correct horse").unwrap();
        let mut there = Store::open_in_memory().unwrap();
        let mut mine = account("ada@example.com", Auth::Password);
        mine.label = Some("Mine".into());
        there.save_account(&mine).unwrap();
        let failing = Failing {
            unwritable: "ada@example.com",
            ..Failing::default()
        };
        failing
            .set(&password_key(&mine.id), &SecretString::from("old"))
            .unwrap();

        // Its IMAP password is written, then its SMTP one can't be.
        let imported = profile.import(&mut there, &failing, EVERYTHING).unwrap();
        assert_eq!(imported.added, ["ada@gmail.com"], "the others are imported");
        assert_eq!(imported.failed.len(), 1);
        assert_eq!(imported.failed[0].0, "ada@example.com");
        assert_eq!(there.account(&mine.id).unwrap(), Some(mine.clone()));
        assert_eq!(
            secret(&failing, &password_key(&mine.id)).as_deref(),
            Some("old"),
            "its password is put back"
        );
        assert_eq!(Settings::load(&there).unwrap().theme, Theme::Dark);
    }

    #[test]
    fn only_what_an_account_logs_in_with_is_exported() {
        let (store, _) = set_up();
        let profile = Profile::collect(&store, &Echo).unwrap();
        let [password, google] = profile.accounts.as_slice() else {
            panic!("two accounts");
        };
        assert!(password.secrets.password.is_some());
        assert!(password.secrets.oauth_refresh_token.is_none(), "no token");
        assert!(google.secrets.oauth_refresh_token.is_some());
        assert!(google.secrets.password.is_none(), "no password");
        assert!(google.secrets.smtp_password.is_none(), "no SMTP password");
    }

    #[test]
    fn a_password_command_takes_accounts_but_not_google_sign_ins() {
        let (store, secrets) = set_up();
        let file = sealed(&store, &secrets);
        let mut there = Store::open_in_memory().unwrap();
        let error = open(&file, "correct horse")
            .unwrap()
            .import(&mut there, &ReadOnly, EVERYTHING)
            .unwrap_err();
        assert!(error.to_string().contains("keyring"), "got {error}");
        assert!(there.accounts().unwrap().is_empty(), "nothing is imported");
        assert_eq!(
            Settings::load(&there).unwrap().theme,
            Theme::System,
            "not even the settings"
        );

        store.remove_account("ada@gmail.com").unwrap();
        let file = sealed(&store, &secrets);
        let mut there = Store::open_in_memory().unwrap();
        let imported = open(&file, "correct horse")
            .unwrap()
            .import(&mut there, &ReadOnly, EVERYTHING)
            .unwrap();
        assert_eq!(imported.without_secrets, ["ada@example.com"]);
        assert_eq!(there.accounts().unwrap().len(), 1);
    }
}
