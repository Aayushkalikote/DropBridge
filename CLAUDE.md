# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

DropBridge is a single macOS-only Rust binary (macOS 26+). It makes Android phones show up in Finder's AirDrop and makes the Mac show up in Android's Quick Share, over the same Wi-Fi.

## Commands

`protoc` is required to build: `rqs_lib` generates its protocol code with it (`brew install protobuf`).

```sh
cargo test --locked                          # what CI runs, plus cargo build --release --locked
cargo test archive::tests::<name>            # one test; unit tests live only in archive.rs, discovery.rs, qr.rs and update.rs
cargo run --release -- --test-peer           # adds a "dropbridge test" AirDrop device; checks the Mac half without a phone
RUST_LOG=dropbridge=debug cargo run --release
cargo run --release -- doctor                # setup checks + lists Quick Share devices in reach
```

Changes that touch AirDrop or Quick Share behaviour need a real Mac and Android phone on the same Wi-Fi; the PR should say what was tested on.

## Architecture

`main.rs` owns the event loop and wires two independent directions together.

**Mac → Android (AirDrop side, written here):**
1. `discovery.rs` browses mDNS for Quick Share (`_FC9F5ED42C8A._tcp`). Devices are keyed by **IPv4**, because the Quick Share endpoint id rotates every few minutes. Removal waits `LOST_GRACE` so the Finder entry does not flicker. Phones in Everyone mode usually advertise the *hidden* form, so `Event::Found.name` is often `None`, unless the phone scanned the `dropbridge qr` code: `qr.rs` derives keys from `qr.key` and decrypts the name from the endpoint info's QR TLV record (`Event::Found.qr` = the phone scanned our code). `dropbridge qr <file>...` runs its own invisible `RQS` and sends to that phone itself, because the phone waits for the sender to connect.
2. `main::on_discovery` maps each device to a `Peer`. Same IP and name: update the shared `addr` in place. Name changed: re-advertise. Missing names fall back to `names.rs` (`~/Library/Application Support/dropbridge/names.tsv`), learned from inbound transfers.
3. `advertise_peer` gives each peer its own TLS listener on AWDL (`airdrop::bind` sets `SO_RECV_ANYIF`). It also registers `_airdrop._tcp` through the dns_sd FFI in `bonjour.rs`. Each peer gets its **own hostname** (AAAA record → this Mac's `awdl0` address), because sharingd ignores peers announced under the Mac's own hostname. The service tag is a hash of (ip, name, this Mac's hostname), so Finder keeps one entry across restarts and two Macs bridging one phone do not claim the same hostname.
4. `airdrop.rs` is a fake AirDrop receiver speaking sharingd's HTTPS protocol: `POST /Discover`, `/Ask`, `/Upload`. It drops connections that don't come from this Mac (its `awdl0` address or loopback), so nearby Macs never list or send through its peers. `archive.rs` streams the upload (dvzip frames → zlib → odc cpio) into the spool dir (`~/Library/Caches/dropbridge/<tag>`). It rejects unsafe paths.
5. The peer's `Forward` closure calls `send_files`, which sends via `rqs_lib` and waits on its broadcast channel for Finished/Rejected/etc. Finder gets its HTTP response only after the phone accepts or declines (`SEND_TIMEOUT`).

**Android → Mac (Quick Share side, via `rqs_lib`):** `RQS` runs the receiver. `main::inbound` watches the channel and asks Accept/Decline through an `osascript` dialog (unless `--auto-accept`). On finish it posts a notification. Sender names flow back to `names.rs` via the `learn` channel.

`update.rs` checks the GitHub `releases/latest` redirect with `/usr/bin/curl` a minute after start and then daily, and notifies once per version (remembered in `update-notified`). It only works for releases whose tag matches `version` and has no `-suffix`.

`tls.rs` creates a self-signed RSA cert in `~/Library/Application Support/dropbridge` with `/usr/bin/openssl`. `-addext` is required: without it LibreSSL makes an X.509 v1 cert, which rustls rejects.

## Dependency pins

- `rqs_lib` is pinned to a git rev of Martichou/rquickshare with `default-features = false`.
- `mdns-sd` must stay on the same Martichou fork/branch that `rqs_lib` uses, so only one copy builds. Bump both together.

## Release and packaging

- Every change adds a changelog entry in README.md's **Changelog** section, under `### vX.Y.Z`. Each entry starts with `Breaking:`, `Feature:`, `Enhancement:`, `Fix:`, `Security:` or `Docs:`. `.github/release-notes.sh` turns them into release notes and fails the release on an untyped entry.
- To release: set `version` in `Cargo.toml`, then push tag `vX.Y.Z`. The tag must match the version. A `-suffix` tag becomes a pre-release. `release.yml` builds a universal (arm64 + x86_64) binary.
- `packaging/dropbridge.rb` is a copy of the formula in the separate `Aayushkalikote/homebrew-tap` repo. It builds from the source tarball, so update its `url`/`sha256` after tagging. Homebrew is the only supported install path; the binary is unsigned.
