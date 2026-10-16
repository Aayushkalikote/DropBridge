# dropbridge

Send files between your Mac and Android phones the native way on both sides:

- **Mac → Android:** your phone shows up in Finder's **AirDrop** window. Drop a file on it and accept on the phone.
- **Android → Mac:** your Mac shows up in Android's **Quick Share** sheet. Accept on the Mac and the file lands in `~/Downloads`.

There's no app to install on the phone and nothing to pair. dropbridge runs quietly in the background on the Mac.

## Install

```sh
brew install aayushkalikote/tap/dropbridge
brew services start dropbridge
```

`brew services` starts dropbridge now and at every login. If macOS asks whether dropbridge may find devices on your local network, click **Allow**.

## Set up the phone (once)

1. Connect it to the **same Wi-Fi** as the Mac.
2. Open **Quick Share** and set *Who can share with you* to **Everyone**.

Quick Share comes with Google Play Services, so Samsung, Pixel, OnePlus, Motorola and most other Android phones have it.

## Use it

**Mac → Android.** Unlock the phone. Open Finder → AirDrop (or use Share → AirDrop) and drop files on the phone. Accept on the phone. Finder waits on *Sending…* until you do. It shows *Failed* if the phone declines or doesn't answer within 10 minutes.

**Android → Mac.** On the phone, tap Share → Quick Share and pick your Mac. Click **Accept** in the dialog on the Mac and check the PIN matches the phone. The files land in `~/Downloads`.

The first time, a phone may show up as **"Android phone"**. Phones in *Everyone* mode often keep their name private until a sender nearby asks over Bluetooth, which a Mac can't do. Send one file from the phone to the Mac and dropbridge learns and remembers its name.

## If the phone doesn't show up

```sh
dropbridge doctor
```

This command checks Wi-Fi, AirDrop and the background service, and lists the Quick Share devices it can see. The usual causes are:

- **The phone is asleep.** Android stops announcing Quick Share while the screen is off. Unlock it and the phone reappears within a few seconds.
- **The phone and Mac are on different networks.** Guest Wi-Fi and networks with "client isolation" also block it.
- **macOS blocked network access.** Check System Settings → Privacy & Security → Local Network and allow dropbridge.

## Options

```
dropbridge [--auto-accept] [--download-dir <path>] [--test-peer]
dropbridge doctor
```

| Option | Effect |
| --- | --- |
| `--auto-accept` | Accept incoming Quick Share files without asking. Anyone on your Wi-Fi could then send you files. |
| `--download-dir <path>` | Where incoming files go (default `~/Downloads`). |
| `--test-peer` | Adds a "dropbridge test" device to AirDrop that saves what you drop on it. It checks the Mac side without a phone. |

To use an option with the background service, run `brew services stop dropbridge` and start `dropbridge` with the option yourself.

## Limits

- **Same Wi-Fi only.** Android's Quick Share can also use Bluetooth and Wi-Fi Direct, but macOS doesn't let apps use those.
- **Mac ↔ Android only.** An iPhone can't send to the phone, because iOS doesn't let apps add devices to its AirDrop list.
- **Unofficial.** dropbridge speaks AirDrop and Quick Share the way their makers do, but neither Apple nor Google supports it. An OS update can break it.
- **Finder sends folders as separate files.** Quick Share has no folders.

## How it works

For each Quick Share device on your network, dropbridge advertises a matching AirDrop receiver on the Mac's AWDL interface. macOS's own AirDrop service lists it in Finder like any nearby device. When you drop a file, Finder uploads it to dropbridge over HTTPS. dropbridge unpacks the upload and sends it to the phone over Quick Share. It answers Finder only after the phone has accepted or declined.

The Quick Share side uses [rqs_lib](https://github.com/Martichou/rquickshare) from RQuickShare.

## Development

```sh
cargo test
cargo run --release -- --test-peer          # Mac side without a phone
RUST_LOG=dropbridge=debug cargo run --release
```

## License

GPL-3.0-only. AirDrop is a trademark of Apple Inc. Quick Share is a trademark of Google LLC and Samsung Electronics. dropbridge isn't affiliated with any of them.
