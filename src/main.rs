//! dropbridge: Android phones (Quick Share) appear in macOS AirDrop, and the Mac
//! appears in Android's Quick Share sheet.

mod airdrop;
mod archive;
mod bonjour;
mod discovery;
mod names;
mod qr;
mod tls;
mod update;

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rqs_lib::channel::{ChannelAction, ChannelDirection, ChannelMessage, TransferType};
use rqs_lib::{DeviceType, OutboundPayload, SendInfo, State, Visibility, RQS};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// TXT flags sharingd accepts for a non-Apple receiver (OpenDrop's default).
const AIRDROP_FLAGS: u32 = 0x88;
/// How long Finder may wait for the phone's owner to accept.
const SEND_TIMEOUT: Duration = Duration::from_secs(600);

const USAGE: &str = "\
dropbridge: show Android phones in macOS AirDrop, and receive Quick Share on this Mac

USAGE: dropbridge [--auto-accept] [--download-dir <path>] [--test-peer]
       dropbridge doctor      check the setup and list Quick Share devices in reach
       dropbridge qr [<file>...]
                              show a QR code to scan on a phone that doesn't show up in AirDrop;
                              with files, send them as soon as the phone has scanned it

  --auto-accept          accept incoming Quick Share files without asking
  --download-dir <path>  where incoming files go (default: ~/Downloads)
  --test-peer            also show a \"dropbridge test\" device in AirDrop that saves
                         what you drop on it to the download folder (checks the AirDrop half)

The phone must be on the same Wi-Fi as this Mac with Quick Share visible to Everyone.";

struct Options {
    doctor: bool,
    qr: bool,
    /// `qr <file>...`: what to send to the phone that scans the code.
    files: Vec<PathBuf>,
    auto_accept: bool,
    download_dir: Option<PathBuf>,
    test_peer: bool,
}

fn parse_args() -> Result<Options> {
    let mut o = Options { doctor: false, qr: false, files: Vec::new(), auto_accept: false, download_dir: None, test_peer: false };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "doctor" => o.doctor = true,
            "qr" => o.qr = true,
            "--auto-accept" => o.auto_accept = true,
            "--test-peer" => o.test_peer = true,
            "--download-dir" => o.download_dir = Some(args.next().context("--download-dir needs a path")?.into()),
            "-V" | "--version" => {
                println!("dropbridge {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            file if o.qr && !file.starts_with('-') => o.files.push(file.into()),
            other => bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
    }
    Ok(o)
}

/// One Android device shown in AirDrop.
struct Peer {
    name: String,
    addr: Arc<Mutex<String>>,
    _ad: bonjour::Advertisement,
    server: JoinHandle<()>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let opts = parse_args()?;
    if std::env::var_os("RUST_LOG").is_none() {
        // `qr` starts rqs invisible, which makes mdns_sd log a harmless "cannot find such service".
        let mdns = if opts.qr { "off" } else { "error" };
        std::env::set_var("RUST_LOG", format!("info,rqs_lib=warn,mdns_sd={mdns}"));
    }
    tracing_subscriber::fmt::init();

    let home = PathBuf::from(std::env::var_os("HOME").context("HOME not set")?);
    let support = home.join("Library/Application Support/dropbridge");
    let names_path = support.join("names.tsv");
    let qr = qr::load(&support)?;
    if opts.qr {
        return show_qr(qr, opts.files).await;
    }
    if opts.doctor {
        return doctor(names::Names::load(names_path), qr).await;
    }
    let tls = tls::acceptor(&support)?;
    let spool = home.join("Library/Caches/dropbridge");
    if bonjour::awdl_address().is_none() {
        bail!("awdl0 is not available; AirDrop needs Wi-Fi turned on");
    }

    let downloads = opts.download_dir.clone().unwrap_or_else(|| home.join("Downloads"));
    let _test = if opts.test_peer { Some(test_peer(downloads, &tls, &spool)?) } else { None };

    let mut rqs = RQS::new(Visibility::Visible, None, opts.download_dir);
    let (send_tx, _) = rqs.run().await?;
    let mut disc = discovery::start(qr)?;
    let mut names = names::Names::load(names_path);
    let (learn_tx, mut learned) = mpsc::channel(8);
    tokio::spawn(inbound(rqs.message_sender.clone(), opts.auto_accept, learn_tx));
    tokio::spawn(update::watch(support.join("update-notified")));
    log::info!("dropbridge running; waiting for Quick Share devices on this network");

    let mut peers: HashMap<std::net::Ipv4Addr, Peer> = HashMap::new();
    let (mut term, mut int) = (signal(SignalKind::terminate())?, signal(SignalKind::interrupt())?);
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = int.recv() => break,
            ev = disc.recv() => match ev {
                Some(ev) => on_discovery(ev, &mut names, &mut peers, &tls, &spool, &send_tx, &rqs),
                None => break,
            },
            Some((ip, name)) = learned.recv() => {
                // Re-announce under the real name if this device is in AirDrop.
                if names.learn(ip, &name) {
                    if let Some(addr) = peers.get(&ip).map(|p| p.addr.lock().unwrap().clone()) {
                        let ev = discovery::Event::Found { ip, name: None, addr, qr: false };
                        on_discovery(ev, &mut names, &mut peers, &tls, &spool, &send_tx, &rqs);
                    }
                }
            }
        }
    }
    log::info!("stopping");
    peers.clear(); // unregisters from AirDrop
    tokio::time::sleep(Duration::from_millis(300)).await; // let mDNS goodbyes go out
    rqs.stop().await;
    Ok(())
}

fn on_discovery(
    ev: discovery::Event,
    names: &mut names::Names,
    peers: &mut HashMap<std::net::Ipv4Addr, Peer>,
    tls: &tokio_rustls::TlsAcceptor,
    spool: &std::path::Path,
    send_tx: &mpsc::Sender<SendInfo>,
    rqs: &RQS,
) {
    let (ip, name, addr) = match ev {
        discovery::Event::Lost { ip } => {
            if let Some(p) = peers.remove(&ip) {
                log::info!("{} left", p.name);
            }
            return;
        }
        discovery::Event::Found { ip, name, addr, .. } => (ip, name, addr),
    };
    let name = match name {
        Some(n) => {
            names.learn(ip, &n);
            n
        }
        // Hidden phones carry no name or brand; the IP's last byte tells several apart.
        None => names.get(&ip).map_or_else(|| format!("Android phone {}", ip.octets()[3]), str::to_string),
    };
    if let Some(p) = peers.get(&ip) {
        if p.name == name {
            // Same device, new Quick Share endpoint (they rotate): keep the AirDrop entry.
            *p.addr.lock().unwrap() = addr;
            return;
        }
        peers.remove(&ip); // name changed (e.g. hidden -> visible): re-advertise
    }
    match add_peer(ip, &name, &addr, tls, spool, send_tx, rqs) {
        Ok(p) => {
            log::info!("{name} ({ip}) is now in AirDrop");
            peers.insert(ip, p);
        }
        Err(e) => log::warn!("could not add {name} to AirDrop: {e:#}"),
    }
}

fn add_peer(
    ip: std::net::Ipv4Addr,
    name: &str,
    addr: &str,
    tls: &tokio_rustls::TlsAcceptor,
    spool: &std::path::Path,
    send_tx: &mpsc::Sender<SendInfo>,
    rqs: &RQS,
) -> Result<Peer> {
    let addr = Arc::new(Mutex::new(addr.to_string()));
    let forward: airdrop::Forward = {
        let (addr, name, send_tx, msgs) = (addr.clone(), name.to_string(), send_tx.clone(), rqs.message_sender.clone());
        Arc::new(move |files| {
            let target = addr.lock().unwrap().clone();
            Box::pin(send_files(send_tx.clone(), msgs.clone(), name.clone(), target, files))
        })
    };
    let (ad, server) = advertise_peer(&ip.to_string(), name, forward, tls, spool)?;
    Ok(Peer { name: name.to_string(), addr, _ad: ad, server })
}

/// Show `name` in AirDrop; files dropped on it go to `forward`.
fn advertise_peer(
    key: &str,
    name: &str,
    forward: airdrop::Forward,
    tls: &tokio_rustls::TlsAcceptor,
    spool: &std::path::Path,
) -> Result<(bonjour::Advertisement, JoinHandle<()>)> {
    // Stable per device so Finder keeps one entry across restarts. The Mac's hostname is
    // mixed in so two Macs bridging the same phone don't fight over one AirDrop hostname.
    let mut host = [0u8; 256];
    // SAFETY: writes at most host.len() bytes into host.
    unsafe { libc::gethostname(host.as_mut_ptr().cast(), host.len()) };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (key, name, host).hash(&mut h);
    let tag = format!("{:012x}", h.finish() & 0xffff_ffff_ffff);
    let listener = airdrop::bind()?;
    let port = listener.local_addr()?.port();
    let receiver = airdrop::Receiver { name: format!("DropBridge - {name}"), model: "Android".into(), spool: spool.join(&tag), forward };
    let ad = bonjour::advertise(&tag, &format!("dropbridge-{tag}"), port, AIRDROP_FLAGS)?;
    log::debug!("{name}: service {tag}._airdrop._tcp, host dropbridge-{tag}.local, port {port}");
    Ok((ad, tokio::spawn(airdrop::serve(listener, tls.clone(), Arc::new(receiver)))))
}

/// `--test-peer`: an AirDrop device that just moves received files into `dir`.
fn test_peer(dir: PathBuf, tls: &tokio_rustls::TlsAcceptor, spool: &std::path::Path) -> Result<(bonjour::Advertisement, JoinHandle<()>)> {
    let forward: airdrop::Forward = Arc::new(move |files| {
        let dir = dir.clone();
        Box::pin(async move {
            for f in files {
                let to = dir.join(f.file_name().context("file without a name")?);
                tokio::fs::copy(&f, &to).await?;
                log::info!("test peer saved {}", to.display());
            }
            Ok(())
        })
    });
    advertise_peer("test", "dropbridge test", forward, tls, spool)
}

/// Send files over Quick Share; resolves when the phone finished receiving them.
async fn send_files(
    send_tx: mpsc::Sender<SendInfo>,
    msgs: broadcast::Sender<ChannelMessage>,
    name: String,
    addr: String,
    files: Vec<PathBuf>,
) -> Result<()> {
    let mut rx = msgs.subscribe();
    let files = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    send_tx
        .send(SendInfo { id: addr.clone(), name: name.clone(), addr: addr.clone(), ob: OutboundPayload::Files(files) })
        .await?;
    let wait = async {
        loop {
            let m = match rx.recv().await {
                Ok(m) => m,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => return Err(e.into()),
            };
            if m.id != addr || m.direction != ChannelDirection::LibToFront {
                continue;
            }
            match m.state {
                Some(State::Finished) => return Ok(()),
                Some(State::Rejected) => bail!("declined on {name}"),
                Some(State::Cancelled) => bail!("cancelled on {name}"),
                Some(State::Disconnected) => bail!("{name} disconnected"),
                _ => {}
            }
        }
    };
    tokio::time::timeout(SEND_TIMEOUT, wait).await.context("no answer from the phone")?
}

/// Android -> Mac: ask before accepting, notify when saved.
async fn inbound(msgs: broadcast::Sender<ChannelMessage>, auto_accept: bool, learn: mpsc::Sender<(std::net::Ipv4Addr, String)>) {
    let mut rx = msgs.subscribe();
    let mut asked = HashSet::new();
    loop {
        let m = match rx.recv().await {
            Ok(m) => m,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return,
        };
        if m.direction != ChannelDirection::LibToFront || m.rtype != Some(TransferType::Inbound) {
            continue;
        }
        let meta = m.meta.clone().unwrap_or_default();
        let from = meta.source.as_ref().map_or("A device".to_string(), |s| s.name.clone());
        let files = meta.files.clone().unwrap_or_default();
        // Inbound ids are the sender's "ip:port"; a sender always includes its name.
        // Computers are skipped: they never show in AirDrop, and their name would stick to the IP.
        if let (Some(src), Some(Ok(ip))) = (&meta.source, m.id.split(':').next().map(str::parse)) {
            if src.device_type != DeviceType::Laptop {
                let _ = learn.try_send((ip, src.name.clone()));
            }
        }
        match m.state {
            Some(State::WaitingForUserConsent) if asked.insert(m.id.clone()) => {
                let msgs = msgs.clone();
                tokio::spawn(async move {
                    let pin = meta.pin_code.unwrap_or_default();
                    let ok = auto_accept
                        || ask(&format!("{from} wants to send you:\n\n{}\n\nPIN: {pin}", files.join("\n"))).await;
                    let action = if ok { ChannelAction::AcceptTransfer } else { ChannelAction::RejectTransfer };
                    let _ = msgs.send(ChannelMessage {
                        id: m.id,
                        direction: ChannelDirection::FrontToLib,
                        action: Some(action),
                        ..Default::default()
                    });
                });
            }
            Some(State::Finished) => {
                asked.remove(&m.id);
                notify("Quick Share", &format!("Received {} file(s) from {from}", files.len())).await;
            }
            Some(State::Rejected | State::Cancelled | State::Disconnected) => {
                asked.remove(&m.id);
            }
            _ => {}
        }
    }
}

/// Accept/Decline dialog; text is passed as an argument so it needs no escaping.
async fn ask(text: &str) -> bool {
    let out = tokio::process::Command::new("/usr/bin/osascript")
        .args([
            "-e", "on run argv",
            "-e", r#"display dialog (item 1 of argv) with title "Quick Share" buttons {"Decline", "Accept"} default button "Accept" giving up after 60"#,
            "-e", "end run",
            text,
        ])
        .output()
        .await;
    matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).contains("button returned:Accept"))
}

async fn notify(title: &str, text: &str) {
    let _ = tokio::process::Command::new("/usr/bin/osascript")
        .args(["-e", "on run argv", "-e", "display notification (item 2 of argv) with title (item 1 of argv)", "-e", "end run", title, text])
        .status()
        .await;
}

/// `dropbridge qr`: scanning the code makes a phone advertise itself, name included, so the
/// running service shows it in AirDrop. The phone then waits for the Mac to connect, so with
/// files this sends them to it straight away.
async fn show_qr(qr: qr::Key, files: Vec<PathBuf>) -> Result<()> {
    let files = files.into_iter().map(|f| f.canonicalize().with_context(|| format!("{}", f.display()))).collect::<Result<Vec<_>>>()?;
    let url = qr.url();
    print!("{}", qr::render(&url)?);
    println!("\nScan this with the phone's camera app, on the same Wi-Fi as this Mac.");
    if files.is_empty() {
        println!("The phone then shows in AirDrop; drop files on it while its Quick Share screen is open.");
        println!("Code: {url}");
        return Ok(());
    }
    println!("Code: {url}\nWaiting for the phone (Ctrl-C to stop)...");
    // Invisible: only sends, so this Mac isn't advertised twice next to the service.
    let mut rqs = RQS::new(Visibility::Invisible, None, None);
    let (send_tx, _) = rqs.run().await?;
    let mut disc = discovery::start(qr)?;
    let (name, addr) = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            match disc.recv().await {
                Some(discovery::Event::Found { name, addr, qr: true, .. }) => return Ok((name.unwrap_or("the phone".into()), addr)),
                Some(_) => {}
                None => bail!("device discovery stopped"),
            }
        }
    })
    .await
    .context("no phone scanned the code within 5 minutes")??;
    println!("Sending {} file(s) to {name}; accept on the phone...", files.len());
    let sent = send_files(send_tx, rqs.message_sender.clone(), name.clone(), addr, files).await;
    rqs.stop().await;
    sent?;
    println!("Sent to {name}.");
    Ok(())
}

/// `dropbridge doctor`: plain-language checks for the usual reasons a phone doesn't show up.
async fn doctor(names: names::Names, qr: qr::Key) -> Result<()> {
    let check = |good: bool, what: &str, fix: &str| {
        println!("{} {what}", if good { "✓" } else { "✗" });
        if !good {
            println!("    → {fix}");
        }
    };
    check(bonjour::awdl_address().is_some(), "AirDrop's Wi-Fi link (awdl0) is up", "Turn Wi-Fi on. AirDrop needs it.");
    check(
        discovery::local_ipv4s().iter().any(|ip| ip.is_private()),
        "This Mac is on a local network",
        "Join the same Wi-Fi network as your phone.",
    );
    let me = std::process::id().to_string();
    let pids = std::process::Command::new("/usr/bin/pgrep").args(["-x", "dropbridge"]).output()?.stdout;
    check(
        String::from_utf8_lossy(&pids).lines().any(|p| p != me),
        "The dropbridge service is running",
        "Start it: brew services start dropbridge",
    );
    if let Some(v) = update::available().await {
        check(false, &format!("DropBridge {v} is available (this is {})", env!("CARGO_PKG_VERSION")), "Update: brew upgrade dropbridge");
    }

    println!("\nLooking for Quick Share devices for 8 seconds...");
    let mut rx = discovery::start(qr)?;
    let mut found = HashMap::new();
    let wait = tokio::time::sleep(Duration::from_secs(8));
    tokio::pin!(wait);
    loop {
        tokio::select! {
            _ = &mut wait => break,
            Some(ev) = rx.recv() => if let discovery::Event::Found { ip, name, .. } = ev {
                found.insert(ip, name);
            },
        }
    }
    for (ip, name) in &found {
        match (name, names.get(ip)) {
            (Some(n), _) => println!("  • {n} ({ip})"),
            (None, Some(n)) => println!("  • {n} ({ip}, hidden; name remembered)"),
            (None, None) => println!("  • Android phone ({ip}, hidden; send one file from it to learn its name)"),
        }
    }
    if found.is_empty() {
        println!("✗ No Quick Share devices found. On the phone:");
        println!("    → Use the same Wi-Fi as this Mac.");
        println!("    → Unlock it and keep the screen on (Android hides Quick Share while asleep).");
        println!("    → Quick Share → Who can share with you → Everyone.");
        println!("    → On the Mac: System Settings → Privacy & Security → Local Network → allow dropbridge.");
        println!("    → Still nothing? Run `dropbridge qr` and scan the code with the phone.");
    }
    Ok(())
}
