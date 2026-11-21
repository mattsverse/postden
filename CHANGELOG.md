# Changelog

## [0.11.0](https://github.com/mattsverse/postden/compare/v0.10.0...v0.11.0) (2026-11-21)


### Features

* **desktop:** edit an account's IMAP and SMTP settings ([820967f](https://github.com/mattsverse/postden/commit/820967fe7d472dbbdc37d37ddc1ba9ddc090ffb0))


### Bug Fixes

* **desktop:** keep read marks as asked while the server answers ([da67a31](https://github.com/mattsverse/postden/commit/da67a31f89bfa8af04785ccf2339b6bd6714fdcc))

## [0.10.0](https://github.com/mattsverse/postden/compare/v0.9.0...v0.10.0) (2026-11-20)


### Features

* autosort inbox mail into the mailboxes you choose ([309ed2d](https://github.com/mattsverse/postden/commit/309ed2d87c5e942e0db3cee45d5a571a794022e4))
* delete a mailbox from its menu ([bc9f501](https://github.com/mattsverse/postden/commit/bc9f5016c4206e2e1d66fded77e81c7850030507))
* delete mailboxes you made, never one the app or server keeps mail in ([774c8e3](https://github.com/mattsverse/postden/commit/774c8e3e92da2a026889ffa1f3cbcc2330f28857))
* download any mailbox ahead, not just the inbox, so its mail opens without waiting ([71d86ba](https://github.com/mattsverse/postden/commit/71d86bafa5ae2c496758ed942433f949b0252092))
* download other mailboxes ahead, as chosen ([5e71ca7](https://github.com/mattsverse/postden/commit/5e71ca7ec4af348ebe298d82f17c745c4f5e1b5c))
* file inbox mail into the mailboxes you choose, by itself ([5f09550](https://github.com/mattsverse/postden/commit/5f0955039d2c76957ade19e16331a67c110f6104))
* group mailboxes with nothing unread in the sidebar ([335f920](https://github.com/mattsverse/postden/commit/335f9200d80e029efa49f1559b6f767b03d1a467))
* keep mailboxes with nothing unread out of the way, so new mail stands out ([4bc527f](https://github.com/mattsverse/postden/commit/4bc527fc3c51cff5efe2fe41fe74ec0bc1d850c3))
* learn where mail goes and what matters, and suggest it ([f949c87](https://github.com/mattsverse/postden/commit/f949c87eee85c5f3f4ecc05fc0e679966c0f9113))
* show an unread count on every mailbox ([07e79ef](https://github.com/mattsverse/postden/commit/07e79ef8c2c96996fa63ba8997d126bb9f6f5635))


### Bug Fixes

* **ai:** learn at once from mail filed by hand in this app ([b066eae](https://github.com/mattsverse/postden/commit/b066eae64c9d504d33bb953a1e6d3ddfea9b6c09))
* **app:** keep debug builds' mail cache apart from the installed app's ([770e44b](https://github.com/mattsverse/postden/commit/770e44bb352b1d45d8af7ff4cb103f0fe878585f))
* learn from mail you file right away, so a new mailbox gets suggested and autosorted ([e43b8a9](https://github.com/mattsverse/postden/commit/e43b8a9050287341080d2d07c491ddddfb42fab2))

## [0.9.0](https://github.com/mattsverse/postden/compare/v0.8.0...v0.9.0) (2026-11-18)


### Features

* add your Thunderbird accounts without typing their servers again ([af552aa](https://github.com/mattsverse/postden/commit/af552aaa3a4a665a46dd382553d661e445cdcb33))
* **app:** find unread mail older than the cache on the server ([d917016](https://github.com/mattsverse/postden/commit/d9170160adba18c16d9920306453e492e15b80f1))
* **app:** list unread mail older than the first 200, so inbox zero means zero on the server ([c7b00dc](https://github.com/mattsverse/postden/commit/c7b00dc13208f3c9ab9bc7dcd4dd1c58c74e9acd))
* **app:** read accounts set up in Apple Mail ([b33fdbf](https://github.com/mattsverse/postden/commit/b33fdbf0dca5e31971e2cbd13d9577a37e86410a))
* **app:** read accounts set up in Thunderbird ([6cba6f0](https://github.com/mattsverse/postden/commit/6cba6f022d6ab5f125ae62ee0e2b072edbb8c460))
* **app:** refresh every mailbox, each showing it's busy until synced ([f83e980](https://github.com/mattsverse/postden/commit/f83e980aa25d521b2623adad8ae9ebb586d07ca0))
* **desktop:** pick the app to set accounts up from ([4e384f7](https://github.com/mattsverse/postden/commit/4e384f7b769d3765d1d346a4c67273f9e1e18431))
* **desktop:** pick the From account from a searchable list ([bd7fd9e](https://github.com/mattsverse/postden/commit/bd7fd9ed640dea863f87882ae1b8095a5198ce46))
* **desktop:** pick who you send from, however many accounts you have ([8e955a8](https://github.com/mattsverse/postden/commit/8e955a8878aeff9fd2d7eb2762bb9696ed6cebd6))
* **desktop:** show only unread mail, down to inbox zero ([3fe42a5](https://github.com/mattsverse/postden/commit/3fe42a535d4ce421d9881b6b19ebfe17b431b93b))
* refresh every mailbox at once, each with a spinner until it's synced ([fdc3371](https://github.com/mattsverse/postden/commit/fdc3371824c88e18a8f43306f124d057f55f02ba))
* suggest people you've written to as you type a recipient ([60379ce](https://github.com/mattsverse/postden/commit/60379cea7fe1562899894f5df7312468f0ce2731))
* suggest remembered contacts while typing recipients ([ebb6881](https://github.com/mattsverse/postden/commit/ebb688140fcc2b85690d20b8f4b4d78fac65ff02))


### Bug Fixes

* **app:** list an iCloud mailbox under its iCloud address ([2308389](https://github.com/mattsverse/postden/commit/2308389e638298f50a4edce16a3905e56962ad76))
* **app:** open the Full Disk Access settings with `open` ([26ca6c4](https://github.com/mattsverse/postden/commit/26ca6c4c417ca07b981b69a598929758801f4470))

## [0.8.0](https://github.com/mattsverse/postden/compare/v0.7.0...v0.8.0) (2026-11-18)


### Features

* add a nightly release track ([42a190b](https://github.com/mattsverse/postden/commit/42a190ba8d54576efdfa4d1bc88cb838bb8881ec))
* **app:** read accounts set up in Spark ([9c92051](https://github.com/mattsverse/postden/commit/9c92051c2b64e31b571b09eb77899f776e6937ad))
* **desktop:** set up accounts from Spark in the add-account form ([b8d290a](https://github.com/mattsverse/postden/commit/b8d290a7ba81146e18615d8c56b6482d78abadcd))
* take mail out of spam, and mark spam for the server's filter ([2937538](https://github.com/mattsverse/postden/commit/29375380231a050914901f31a25798755bc856e9))

## [0.7.0](https://github.com/mattsverse/postden/compare/v0.6.0...v0.7.0) (2026-11-18)


### Features

* **desktop:** create mailboxes from an account's menu ([3ff3841](https://github.com/mattsverse/postden/commit/3ff38411d7cabde887c607cce08a1ddd3eaa3d71))
* **desktop:** create your own mailboxes from an account's menu ([b1a0d31](https://github.com/mattsverse/postden/commit/b1a0d313d79508ed45ca866ede417195f8513983))
* **desktop:** refresh a mailbox from its menu ([d1f98c1](https://github.com/mattsverse/postden/commit/d1f98c11188e53ec570d461b158743ee99efb53d))
* **desktop:** refresh what's shown from the palette or with ⇧⌘N ([5a791fb](https://github.com/mattsverse/postden/commit/5a791fbe3014f6936ada6c3f1c4641e53f519bbe))
* keep opened messages downloaded and clear downloaded mail ([b233007](https://github.com/mattsverse/postden/commit/b2330071bc7bbbcd2f41a2587caf29e8b31ce5e3))

## [0.6.0](https://github.com/mattsverse/postden/compare/v0.5.0...v0.6.0) (2026-11-17)


### Features

* **desktop:** add a command palette ([0aa4fea](https://github.com/mattsverse/postden/commit/0aa4fea6a3e0e0164388d244962258c599ad742f))
* **desktop:** archive, delete, star and forward mail, and undo ([a1660d1](https://github.com/mattsverse/postden/commit/a1660d1c00c1623e6f96c2ae1da8a269e0cba8bd))
* **desktop:** move through mail with single keys ([24b227d](https://github.com/mattsverse/postden/commit/24b227d5b50ab07d0faec09b1df9d21d1d8e5f62))
* **desktop:** split the settings into General, Accounts and Advanced sections ([656ae23](https://github.com/mattsverse/postden/commit/656ae233a0f6561a259a9aa8dc6ad244fca74dbf))
* **desktop:** triage mail from the keyboard, with undo ([f5c7db7](https://github.com/mattsverse/postden/commit/f5c7db7e89f0b6249a13eaa7b60b10a57bdd0fb7))
* snooze messages until later ([2ea941f](https://github.com/mattsverse/postden/commit/2ea941f94e11611e560595c0ae891a348bb70101))

## [0.5.0](https://github.com/mattsverse/postden/compare/v0.4.0...v0.5.0) (2026-11-16)


### Features

* download the newest inbox messages ahead of opening them ([896cef5](https://github.com/mattsverse/postden/commit/896cef565fc9acfd39f570707a7047f815170dcd))
* export a message as an .eml file ([62234c4](https://github.com/mattsverse/postden/commit/62234c4ece646677004ba11c22eedfe1fb4e861f))
* move accounts and settings to another computer with a passphrase-locked file ([69d73f1](https://github.com/mattsverse/postden/commit/69d73f153104ad3314c8b093af4f21b9b769b42e))
* send accounts and settings straight to another computer with a short code ([80c58ea](https://github.com/mattsverse/postden/commit/80c58ea8bbfebdfbc8fa2a5fac033c934982d62c))
* show when marking a mailbox read is running and when it finishes ([b6362e5](https://github.com/mattsverse/postden/commit/b6362e575ae5c92c7b2d701dbd0df008895f3477))

## [0.4.0](https://github.com/mattsverse/postden/compare/v0.3.0...v0.4.0) (2026-11-15)


### Features

* add a context menu to messages in the list ([fd34ef9](https://github.com/mattsverse/postden/commit/fd34ef90a9de9944dd744b03d4973eb46832ae20))
* announce new mail with system notifications ([eff792f](https://github.com/mattsverse/postden/commit/eff792fb546603765e834d7d98e1a1502412b1a1))
* list the inbox first under each account and add a mailbox context menu ([5c4e6c6](https://github.com/mattsverse/postden/commit/5c4e6c6a598186551ba81849d9e9e95d70c532c9))

## [0.3.0](https://github.com/mattsverse/postden/compare/v0.2.0...v0.3.0) (2026-11-15)


### Features

* keep running in the tray and badge unread mail ([13bc0c0](https://github.com/mattsverse/postden/commit/13bc0c0398ccc07d96703f254af8fff1ea25add3))

## [0.2.0](https://github.com/mattsverse/postden/compare/v0.1.0...v0.2.0) (2026-11-15)


### Features

* search mail locally and on the server ([0222bf1](https://github.com/mattsverse/postden/commit/0222bf194bd37ec9d43b8855ca1014c156ec8423))

## 0.1.0 (2026-11-15)


### Features

* initial commit ([dbd5810](https://github.com/mattsverse/postden/commit/dbd5810eb6acbb2361a44b836385fb98604b5712))
