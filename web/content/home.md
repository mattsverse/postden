---
title: "Postden: a private, open-source desktop email client"
description: A free, open-source email client for macOS, Windows and Linux. It connects straight to your IMAP, SMTP or Gmail account, keeps your mail on your computer, and sends no telemetry.
headline: Just an email client.
intro: Postden is a desktop app for IMAP, SMTP and Gmail. It connects to your mail provider directly, keeps your mail on your computer and your passwords in the system keychain. Postden has no server of its own, so there's no account to create.
facts:
  - Free, MIT licensed
  - No telemetry
  - macOS, Windows, Linux
heroCommand: brew install --cask mattsverse/tap/postden
heroCommandNote: Homebrew, on macOS. Windows and Linux downloads are below.
heroScreenshot:
  name: inbox
  alt: "Postden's inbox: every inbox in one list, with mail tagged for the Receipts, News and Travel mailboxes."
  width: 1100
  height: 720
featuresTitle: It all runs on your computer.
featuresIntro: That includes the sorting, which learns from what you do with your mail. What it learns stays on your computer, and you can wipe it or turn it off under Settings → General.
features:
  - kind: sorting
    title: It learns where your mail goes
    body: Postden keeps track of where you file mail, what you leave in the inbox, what you star or delete, and who you write to. Once its guesses are usually right, inbox mail gets a tag naming the mailbox it seems to belong in, and pressing the tag or M moves it there. Mail that looks important is tagged too. Right-click a mailbox and choose Autosort mail here, and Postden moves mail into it by itself when it's sure.
    screenshot: { name: sorting, alt: "The inbox, with mail tagged → Receipts, → News and → Travel.", width: 610, height: 370 }
  - kind: search
    title: Search as you type
    body: Results update with each letter. Postden looks through the subject, sender and recipients of the mail on your computer, in the mailbox you're in or in every inbox. Press Return to search the server too, which finds older mail and words in the message text.
    screenshot: { name: search, alt: "Searching the Receipts mailbox for “invoi”: every Fernbank Hosting invoice, and an option to search the server, in message text too.", width: 610, height: 360 }
  - kind: compat
    title: Check your HTML before you send it
    body: Write HTML in the visual editor or as source. The compose window scores the markup for Gmail, Outlook, Apple Mail, Yahoo and Thunderbird using caniemail.com's data, which ships with the app, so the check works offline.
    screenshot: { name: compat, alt: "The compose window's compatibility check: 55%, with a score for Gmail, Outlook, Apple Mail, Yahoo and Thunderbird and what each won't show.", width: 610, height: 325 }
  - kind: safe
    title: Opening mail tells the sender nothing
    body: Postden turns HTML mail into text before showing it. Scripts are dropped and images appear as their alt text, so nothing is fetched from the web and tracking pixels never load.
    screenshot: { name: alt-text, alt: "A newsletter opened in Postden, with each image shown as its alt text in brackets.", width: 610, height: 415 }
  - kind: transfer
    title: Move to a new computer with a code
    body: Export every account, with its password or Google sign-in, along with your signatures and settings, to one file locked with a passphrase. You can also send it over the network instead. The old computer shows a 16-character code and you type it into the new one.
    screenshot: { name: transfer, alt: "Moving to another computer, in the settings: the code to type on the other computer.", width: 610, height: 235 }
  - kind: eml
    title: Save messages as .eml
    body: Any message can be saved as an .eml file with every header, part and attachment as the server has it.
    screenshot: { name: export, alt: "A message's menu, with Export as .eml….", width: 610, height: 370 }
privacy:
  title: When Postden goes online.
  intro: This is everything the app connects to.
  # Keep in step with "Connections the app makes" in content/pages/privacy.md.
  groups:
    - title: Always
      items:
        - name: Your mail provider
          body: IMAP and SMTP, to fetch, send and manage your mail.
    - title: Sometimes
      items:
        - name: Google
          body: Signing in to Gmail accounts, and keeping them signed in.
        - name: GitHub
          body: When the app starts and once a day, to check for a new release, and to download one you install. Turn it off in Settings → Updates.
        - name: n0
          body: While you send your setup to a new computer. n0's servers help the two computers find each other and relay the connection until they can connect directly. It's end-to-end encrypted.
    - title: Never
      items:
        - name: Us
          body: Postden has no servers.
        - name: Analytics, telemetry, crash reports
          body: The app doesn't collect them.
  website: This website sets no cookies and loads no third‑party scripts.
installTitle: Install in a minute.
installIntro: macOS 13 or later, on Apple silicon or Intel. Windows and Linux on x86_64 and ARM64. Updates download from GitHub and are checked against the release's SHA-256 checksums before they install.
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
