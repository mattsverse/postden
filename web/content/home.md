---
title: "Postden: a private, open-source desktop email client"
description: A free, open-source email client for macOS, Windows and Linux. IMAP, SMTP and Gmail, with your mail kept on your computer and no telemetry.
headline: Your email,
headlineSecond: on your computer.
headlineAccent: Nobody in between.
intro: A native desktop email client for IMAP, SMTP and Gmail. Your mail and passwords stay on your machine, and there's no account to create with us.
facts:
  - Free and open source
  - No telemetry
  - macOS, Windows, Linux
heroCommand: brew install --cask mattsverse/tap/postden
heroCommandNote: With Homebrew on macOS
heroScreenshot:
  name: inbox
  alt: "Postden's inbox: all inboxes in one list, with three unread messages."
  width: 847
  height: 564
featuresTitle: Built for reading, writing and sending mail.
features:
  - icon: compose
    title: HTML compatibility check
    body: Write in HTML and see, before you send, which parts Gmail, Outlook, Apple Mail, Yahoo and Thunderbird won't show.
  - icon: inbox
    title: Any mailbox
    body: IMAP and SMTP with your own credentials, or sign in with Google. No app passwords.
  - icon: shield
    title: Safe HTML mail
    body: Remote images and scripts never load, so senders can't tell when you've opened their mail.
  - icon: code
    title: Open source
    body: MIT licensed. Read the code, or build it yourself.
privacy:
  title: What leaves your computer.
  intro: Every connection Postden makes, listed. That's the whole list.
  never:
    - Analytics
    - Telemetry
    - Crash reports
  website: This website doesn't set cookies or load third‑party scripts. Its fonts are served from here, too.
  # Every host the app itself connects to, as firewall rules. The deny rule is the last line.
  rules:
    - action: allow
      host: your mail provider
      purpose: sign-in, IMAP and SMTP, to fetch and send your mail
      when: always
    - action: allow
      host: github.com
      purpose: checks for new releases and downloads the ones you install
      when: daily, off in Settings
    - action: deny
      host: "*"
      purpose: everything else, including us
      when: Postden has no servers
installTitle: Install in a minute.
installIntro: macOS 13 or later on Apple silicon and Intel. Windows and Linux on x86_64 and ARM64.
install:
  - label: macOS
    os: mac
    note: Homebrew installs the signed, notarized app.
    command: brew install --cask mattsverse/tap/postden
    download: { label: Or get the DMG on GitHub, href: "https://github.com/mattsverse/postden/releases/latest" }
  - label: Windows
    os: windows
    note: Unzip and run postden.exe. The build is unsigned, so SmartScreen may ask you to confirm.
    download: { label: Get the .zip on GitHub, href: "https://github.com/mattsverse/postden/releases/latest" }
  - label: Linux
    os: linux
    note: Needs a Vulkan driver and a Secret Service such as GNOME Keyring.
    variants:
      - label: .deb
        note: Debian, Ubuntu and derivatives.
        download: { label: Get the .deb on GitHub, href: "https://github.com/mattsverse/postden/releases/latest" }
        command: sudo apt install ./postden_*.deb
        steps: true
      - label: .rpm
        note: Fedora, RHEL and derivatives.
        download: { label: Get the .rpm on GitHub, href: "https://github.com/mattsverse/postden/releases/latest" }
        command: sudo dnf install ./postden-*.rpm
        steps: true
      - label: Homebrew
        note: Homebrew on Linux.
        command: brew install mattsverse/tap/postden
---

SHA-256 checksums for every build are published with each
[release](https://github.com/mattsverse/postden/releases/latest).
