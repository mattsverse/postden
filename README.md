# Postden

> local, privacy-focused, no-frills emails

## Features

- smtp & imap support: just bring your credentials and we'll do the rest
- google oauth: connect your gmail account
- search: as you type, through the subject, sender and recipients of the mail on your device, in the mailbox shown or every inbox; press Return to search the server too, which finds older mail and words in message text
- html mail: read it safely (no remote images, no scripts) and write it in a visual editor or as source, with a score for how well Gmail, Outlook, Apple Mail, Yahoo and Thunderbird support the markup
- export: save any message as an `.eml` file, every header, part and attachment as the server holds it
- moving to another computer: export every account, with its password or Google sign-in and signature, and the settings to a passphrase-locked file ([age](https://age-encryption.org)), then import it on the other one, or send it there directly over [iroh](https://iroh.computer) by typing the code this one shows
- sorting, learned on your computer as you go from what you do with your mail (where you file it, what you keep in your inbox, what you star or delete, who you write to): once it's often right, inbox mail is tagged with the mailbox it seems to belong in (press the tag, or M, to move it there) and as important when it seems so. What it learned is kept, can be forgotten or turned off under **Settings → General**, and can go along when moving to another computer, if you tick the box
- optional ai autoreplier

## Versions

- desktop app
- web app

## Install

With [Homebrew](https://brew.sh) on macOS (Apple silicon and Intel), install the signed, notarized app into Applications with the cask:

```sh
brew install --cask mattsverse/tap/postden
```

With Homebrew on Linux (x86_64 and aarch64), use the formula:

```sh
brew install mattsverse/tap/postden
```

On Debian and Ubuntu, or Fedora and RHEL, install the `.deb` or `.rpm` from the [latest release](https://github.com/mattsverse/postden/releases/latest):

```sh
sudo apt install ./postden_<version>_amd64.deb        # or _arm64
sudo dnf install ./postden-<version>-1.x86_64.rpm     # or .aarch64
```

The packages add Postden to the app menu. It needs a Vulkan driver, and a Secret Service such as GNOME Keyring to store passwords.

## Updates

Postden checks GitHub for a new release when it starts and once a day; turn that off, or check by hand, under **Settings → Updates**. A new release is announced with the changelog of every version since yours (the release notes release-please writes). **Skip this version** stops automatic checks from announcing it again.

How it updates depends on how it was installed:

- **macOS app** (from the DMG, the zip, or the Homebrew cask): Postden downloads the release's zip, checks it against the release's `SHA256SUMS`, confirms with `codesign` and `spctl` that it's signed by the same Developer ID team and notarized, replaces the app bundle, and restarts.
- **Linux or Windows archive**: it downloads the archive, checks it against `SHA256SUMS`, and replaces the binary (on Windows with [self-replace](https://crates.io/crates/self-replace)). These builds are unsigned, so the checksum, fetched over HTTPS from the same release, is the only check.
- **Homebrew formula** (Linux): it says to run `brew upgrade postden`.
- **.deb or .rpm** (a binary under `/usr`): it links to the release page.

Debug builds check for updates but never install one. The code is in `crates/update`; it relies on the asset names in `.github/workflows/release.yml`, so keep them in sync.

## Toolchains

`mise.toml` pins Rust, Python, lefthook and nfpm, and `mise.lock` pins their checksums for every CI platform. Run `mise install` locally; CI installs the same versions with `jdx/mise-action` and fails if the lockfile is out of date. After changing a version, refresh the lockfile with the command in `mise.toml`.

## Gmail (Google sign-in)

Gmail accounts sign in with Google in the browser (OAuth 2.0 with PKCE and a loopback redirect) and log in to IMAP and SMTP with `XOAUTH2`; no app password is needed. In the desktop app, press **Sign in with Google** on the add-account form. Run it again to sign in anew if Google ever asks.

The refresh token is kept in the OS keyring, so Google sign-in doesn't work with `POSTDEN_PASSWORD_COMMAND`.

The app needs a Google OAuth client of type **Desktop app**, with the `https://mail.google.com/` scope on its consent screen. Release builds compile one in from the `POSTDEN_GOOGLE_CLIENT_ID` and `POSTDEN_GOOGLE_CLIENT_SECRET` environment variables. Setting them at run time overrides the built-in client, so a build from source can use your own. For local development, copy `.env.example` to `.env` (gitignored) and fill it in; mise loads it in this directory. A desktop client's secret isn't confidential (it ships in the binary), but keep it out of the repository anyway.

While the consent screen is in **Testing**, only its listed test users can sign in, and Google expires their sign-in after 7 days. Lifting both limits means publishing the app, which for this restricted scope requires Google's verification and a yearly security assessment.

## Email client compatibility data

The compose window scores a message's markup against [caniemail.com](https://www.caniemail.com)'s data (MIT), bundled in `crates/html/src/compat/data.rs` so the check runs offline. Refresh it with `python3 scripts/caniemail.py`; to change which mail apps are scored, edit `CLIENTS` there and regenerate.

## Releasing

[release-please](https://github.com/googleapis/release-please) reads Conventional Commits on `main` and opens a release PR that bumps the workspace version in `Cargo.toml` and `Cargo.lock` and updates `CHANGELOG.md`. Merging that PR tags the release and creates a draft. `.github/workflows/release.yml` then builds:

- **macOS**: a universal `Postden.app`, signed with Developer ID, notarized and stapled (as a DMG and a zip)
- **Linux and Windows** (x86_64 and aarch64): unsigned archives with the desktop binary
- **Linux** (x86_64 and aarch64): `.deb` and `.rpm` packages, built with [nfpm](https://nfpm.goreleaser.com) from `assets/linux/nfpm.yaml`

It then uploads them along with `SHA256SUMS`, publishes the release, and points `Formula/postden.rb` and `Casks/postden.rb` in [mattsverse/homebrew-tap](https://github.com/mattsverse/homebrew-tap) at it (`.github/workflows/homebrew.yml`, which `scripts/release.py formula` and `scripts/release.py cask` write from the published `SHA256SUMS`). To redo only the tap update, run the Publish Homebrew formula and cask workflow with the tag.

The bundle identifier, `com.matteogassend.postden`, is set in `assets/macos/Info.plist`, `APP_ID` in `apps/desktop/src/main.rs` (the Linux window's app id), and the name of `assets/linux/com.matteogassend.postden.desktop`; change them together. To retry a failed release, run the Release workflow manually with the existing tag; assets can be replaced only while the release is still a draft.

Repository secrets:

| Secret | Purpose |
| --- | --- |
| `RELEASE_PLEASE_TOKEN` | A fine-grained PAT (contents, pull requests, issues: read/write) so release PRs and releases trigger workflows |
| `APPLE_CERTIFICATE_P12` | Base64 of a P12 holding exactly one Developer ID Application identity |
| `APPLE_CERTIFICATE_PASSWORD` | The P12's password |
| `APPLE_ID` / `APPLE_TEAM_ID` / `APPLE_APP_SPECIFIC_PASSWORD` | Notarization credentials |
| `HOMEBREW_TAP_TOKEN` | A fine-grained PAT with contents read/write on `mattsverse/homebrew-tap` |
| `GOOGLE_OAUTH_CLIENT_ID` / `GOOGLE_OAUTH_CLIENT_SECRET` | The Google OAuth desktop client built into releases, for Gmail sign-in |

Adding a workspace crate? Add its name to the `Cargo.lock` JSONPath in `release-please-config.json`; CI (`scripts/release.py check-config`) fails until you do.

## License

Copyright (C) 2026 Matteo Gassend

Postden is licensed under the [MIT License](LICENSE).
