# Changelog

## [0.1.10](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.9...v0.1.10) (2026-09-14)


### Features

* allow re-pairing and unpairing after setup ([#47](https://github.com/s3ntin3l8/mullion-helper/issues/47)) ([10a30c4](https://github.com/s3ntin3l8/mullion-helper/commit/10a30c426a8f7512c9c5acdc8b9a47c16b560752))


### Bug Fixes

* **macos:** explicitly toggle activation policy around window visibility ([#49](https://github.com/s3ntin3l8/mullion-helper/issues/49)) ([17f3b11](https://github.com/s3ntin3l8/mullion-helper/commit/17f3b114bc1b3bb2b399a799e7afe84e4e9fdb1c))

## [0.1.9](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.8...v0.1.9) (2026-09-14)


### Bug Fixes

* **macos:** match the launchd socket by path shape, not a hardcoded prefix ([#46](https://github.com/s3ntin3l8/mullion-helper/issues/46)) ([88ffc65](https://github.com/s3ntin3l8/mullion-helper/commit/88ffc6578ae1a2bb941f91f84a45c5bd9db204a2))
* **macos:** rank the launchd SSH agent below 1Password in auto-detect ([#43](https://github.com/s3ntin3l8/mullion-helper/issues/43)) ([8f1bfd3](https://github.com/s3ntin3l8/mullion-helper/commit/8f1bfd3951e37619b6287e7284c501a46b216ce9))
* **macos:** set accessory activation policy at runtime ([#45](https://github.com/s3ntin3l8/mullion-helper/issues/45)) ([bb0d750](https://github.com/s3ntin3l8/mullion-helper/commit/bb0d7502adbac92749f2f5600a9e9bac93e5ac48))

## [0.1.8](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.7...v0.1.8) (2026-09-13)


### Bug Fixes

* hide macOS dock icon for tray-only app ([#41](https://github.com/s3ntin3l8/mullion-helper/issues/41)) ([e7cf7d1](https://github.com/s3ntin3l8/mullion-helper/commit/e7cf7d1f23330ed958ccf8b4e9258d02ca469b45))

## [0.1.7](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.6...v0.1.7) (2026-09-12)


### Bug Fixes

* log worker failures to disk and make pairing errors readable ([c5ee7f0](https://github.com/s3ntin3l8/mullion-helper/commit/c5ee7f0e4ebca17adb75369dab8572cd5e3c78c5))
* raise the bridge worker log's rotation budget ([ff66f6f](https://github.com/s3ntin3l8/mullion-helper/commit/ff66f6fe85397cc43f48af479ce38ae107984412))
* sign macOS worker sidecar with JIT entitlement ([d2a8bbe](https://github.com/s3ntin3l8/mullion-helper/commit/d2a8bbef12d070ba80e42229c320a7d5c7cb3f2c))

## [0.1.6](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.5...v0.1.6) (2026-09-11)


### Bug Fixes

* unblock Windows updates and strengthen tray pulse ([#32](https://github.com/s3ntin3l8/mullion-helper/issues/32)) ([8171cbc](https://github.com/s3ntin3l8/mullion-helper/commit/8171cbc5b72f8043ed205f2781ad7d475bbc5ed1))

## [0.1.5](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.4...v0.1.5) (2026-09-11)


### Bug Fixes

* hide Windows worker and show tray status ([#28](https://github.com/s3ntin3l8/mullion-helper/issues/28)) ([fda3e00](https://github.com/s3ntin3l8/mullion-helper/commit/fda3e00ef9fbb4fa529aa55fa5ddbfd8d95be4db))

## [0.1.4](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.3...v0.1.4) (2026-09-10)


### Bug Fixes

* **ci:** move dependency-review to its own pull_request workflow ([#24](https://github.com/s3ntin3l8/mullion-helper/issues/24)) ([d347774](https://github.com/s3ntin3l8/mullion-helper/commit/d3477740f67f1b05366ad20aa491c73600e9d94d))

## [0.1.3](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.2...v0.1.3) (2026-09-10)


### Bug Fixes

* publish complete updater releases ([#22](https://github.com/s3ntin3l8/mullion-helper/issues/22)) ([4709d6a](https://github.com/s3ntin3l8/mullion-helper/commit/4709d6ab18e185260dd69e261963736bacc3a1ae))

## [0.1.2](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.1...v0.1.2) (2026-09-10)


### Bug Fixes

* build installers for path-scoped releases ([#18](https://github.com/s3ntin3l8/mullion-helper/issues/18)) ([b2bedb5](https://github.com/s3ntin3l8/mullion-helper/commit/b2bedb573621b90fc62f53e4463895bc4fe8e42f))
* expose the Tauri CLI to the release action ([#20](https://github.com/s3ntin3l8/mullion-helper/issues/20)) ([94309ad](https://github.com/s3ntin3l8/mullion-helper/commit/94309ad911fee5cd137fac4dbafb86ae010bb149))
* prevent installer matrix skip propagation ([#19](https://github.com/s3ntin3l8/mullion-helper/issues/19)) ([9557e1b](https://github.com/s3ntin3l8/mullion-helper/commit/9557e1b730111528293a53c1808807264c30ec17))

## [0.1.1](https://github.com/s3ntin3l8/mullion-helper/compare/v0.1.0...v0.1.1) (2026-09-10)


### Features

* ship the complete SSH-agent bridge tray app ([#13](https://github.com/s3ntin3l8/mullion-helper/issues/13)) ([6cf96f1](https://github.com/s3ntin3l8/mullion-helper/commit/6cf96f1a4822e70177bae61fc6b7747ca5d2d72b))
