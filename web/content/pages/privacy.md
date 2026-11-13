---
title: Privacy Policy
description: What Postden and this website collect (very little), where your data goes, and your choices.
updated: 2026-11-14
---

Postden is built so that your email is none of our business. This policy explains
what the Postden desktop app and the Postden website do with your data.

## The short version

- We don't run servers that receive your email, contacts, passwords or settings.
- The app has no analytics, telemetry or crash reporting.
- Your mail is stored on your own computer, and your passwords and sign-in tokens are kept
  in your operating system's keychain.
- The app only talks to your mail provider, to Google if you sign in with Google, and to
  GitHub to check for updates (which you can turn off). It has no AI features yet.

## Data the app stores on your computer

Postden downloads your messages from your mail server and keeps them, along with your
account settings, on your device. Passwords, and the refresh token from Google sign-in, are
stored in your operating system's secure credential store (Keychain on macOS, a Secret
Service such as GNOME Keyring on Linux, Credential Manager on Windows), or fetched from a
password command you configure yourself.

This data never leaves your device except as described below. Deleting an account in the app,
or uninstalling the app and its data, removes it.

## Connections the app makes

### Your mail provider

The app connects to the IMAP and SMTP servers you configure to fetch,
send and manage your mail. Your provider's own privacy policy covers what it does with that
data.

### Google, if you sign in with Google

Signing in opens Google's consent page in your browser
and grants Postden access to your Gmail account through the `https://mail.google.com/`
scope. The app uses that access only to read, send and manage your mail on your device, the
same way it uses IMAP and SMTP for any other account. We don't receive, store or share your
Google data, and we don't use it for advertising or to train AI models. Postden's use of
information received from Google APIs adheres to the
[Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy),
including the Limited Use requirements. You can revoke access at any time from your
[Google Account](https://myaccount.google.com/permissions).

### GitHub, to check for updates

When it starts and once a day, the app asks GitHub's API
whether a newer release exists, and downloads it from GitHub if you choose to update. GitHub
sees your IP address and the request, as with any web request. You can turn automatic checks
off under **Settings → Updates**.

### AI features, once they exist

Postden doesn't include AI features yet. Optional email classification and auto-replies are
planned. When they ship they'll be off by default, and if you turn them on, the content of the
messages they process will be sent to the AI provider you configure, under that provider's
terms and privacy policy. We won't receive it, and we'll update this policy before they ship.

### Remote content in emails

HTML messages are shown without remote images or scripts, so
opening a message doesn't tell its sender that you read it.

## This website

The website doesn't use cookies, analytics or third-party trackers. Like any website, its
hosting provider may keep standard server logs, such as IP addresses and the pages requested,
for security and to keep the site running.

## Children

Postden isn't directed at children under 13, and we don't knowingly collect their data.

## Your rights

Because your data stays on your device, it's yours to manage: your mail, accounts and settings
are in a single SQLite file, `postden.db`, which you can copy, back up or delete at any time,
and your passwords are in your system keychain. Where privacy laws such as the GDPR give you rights over personal data we hold, we'll
honor them, though in practice we hold none from the app.

## Changes

We'll update this page if what Postden collects changes, and change the date at the top.
The full history of this policy is public in the
[Postden repository](https://github.com/mattsverse/postden/commits/main/web/content/pages/privacy.md).

## Contact

Questions about privacy? Open an issue on
[GitHub](https://github.com/mattsverse/postden/issues).
