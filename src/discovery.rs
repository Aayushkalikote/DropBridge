//! Find Quick Share receivers on the LAN, including "hidden" ones.
//!
//! Phones in Everyone mode still advertise the hidden form (no name) unless a nearby
//! sender triggers them over BLE, which macOS apps can't do. They accept transfers
//! anyway, so hidden devices are reported too, with `name: None` unless they scanned our QR
//! code (see `qr.rs`), which reveals their name. Devices are keyed by IP:
//! the Quick Share endpoint id rotates every few minutes, the address doesn't.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use anyhow::Result;
use base64::Engine;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use tokio::sync::mpsc;

use crate::qr;

const SERVICE: &str = "_FC9F5ED42C8A._tcp.local.";
/// Devices drop and re-add their advertisement when the endpoint id rotates; only report
/// them gone if they stay gone this long, so the AirDrop entry doesn't flicker.
const LOST_GRACE: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum Event {
    /// `qr`: the device scanned our QR code and is waiting for us to send.
    Found { ip: Ipv4Addr, name: Option<String>, addr: String, qr: bool },
    Lost { ip: Ipv4Addr },
}

pub fn start(qr: qr::Key) -> Result<mpsc::Receiver<Event>> {
    let daemon = ServiceDaemon::new()?;
    let browse = daemon.browse(SERVICE)?;
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let _daemon = daemon; // keep browsing alive
        let mut by_ip: HashMap<Ipv4Addr, HashSet<String>> = HashMap::new();
        let mut leaving: HashMap<Ipv4Addr, Instant> = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            let ev = tokio::select! {
                ev = browse.recv_async() => match ev {
                    Ok(ev) => ev,
                    Err(_) => return,
                },
                _ = tick.tick() => {
                    let gone: Vec<_> = leaving.iter().filter(|(_, t)| t.elapsed() >= LOST_GRACE).map(|(ip, _)| *ip).collect();
                    for ip in gone {
                        leaving.remove(&ip);
                        if tx.send(Event::Lost { ip }).await.is_err() {
                            return;
                        }
                    }
                    continue;
                }
            };
            let out = match ev {
                ServiceEvent::ServiceResolved(info) => {
                    let Some(ip) = info.get_addresses_v4().into_iter().next().copied() else { continue };
                    if local_ipv4s().contains(&ip) {
                        continue; // our own Quick Share receiver
                    }
                    let n = info.get_property_val_str("n");
                    if n.is_some_and(is_computer) {
                        // PCs and Macs running Quick Share apps; Macs already show in AirDrop.
                        log::debug!("skipping computer {} at {ip}", info.get_fullname());
                        continue;
                    }
                    let (name, scanned) = n.map(|n| endpoint(n, &qr)).unwrap_or_default();
                    by_ip.entry(ip).or_default().insert(info.get_fullname().to_string());
                    leaving.remove(&ip);
                    Event::Found { ip, name, addr: format!("{ip}:{}", info.get_port()), qr: scanned }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    let Some(ip) = by_ip.iter_mut().find_map(|(ip, names)| names.remove(&fullname).then_some(*ip)) else {
                        continue;
                    };
                    if !by_ip[&ip].is_empty() {
                        continue; // the device re-advertised under a new endpoint id
                    }
                    by_ip.remove(&ip);
                    leaving.insert(ip, Instant::now());
                    continue
                }
                _ => continue,
            };
            if tx.send(out).await.is_err() {
                return;
            }
        }
    });
    Ok(rx)
}

/// Device type 3 (laptop/desktop) in bits 1..3 of the endpoint info's first byte.
fn is_computer(n: &str) -> bool {
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(n.trim_end_matches('='));
    b.ok().and_then(|b| b.first().copied()).is_some_and(|f| (f >> 1) & 7 == 3)
}

/// Name, and whether the device scanned our QR code, from the base64url endpoint info.
/// Byte 0: flags (0x10 = hidden), bytes 1..17: salt + encrypted key, then for visible devices a
/// length-prefixed name, then TLV records (type, length, value). Type 1 is the QR code record:
/// a visible device puts our advertising token there, a hidden one its name, encrypted.
fn endpoint(n: &str, qr: &qr::Key) -> (Option<String>, bool) {
    let parse = || {
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(n.trim_end_matches('=')).ok()?;
        let mut rest = b.get(17..)?;
        let mut name = None;
        if b[0] & 0x10 == 0 {
            let (&len, tail) = rest.split_first()?;
            name = String::from_utf8(tail.get(..len as usize)?.to_vec()).ok().filter(|n| !n.is_empty());
            rest = &tail[len as usize..];
        }
        while let [t, len, tail @ ..] = rest {
            let Some(value) = tail.get(..*len as usize) else { break };
            if *t == 1 {
                if value == qr.token() {
                    return Some((name, true));
                }
                let hidden = qr.name(value);
                let ours = hidden.is_some();
                return Some((name.or(hidden), ours));
            }
            rest = &tail[value.len()..];
        }
        Some((name, false))
    };
    parse().unwrap_or_default()
}

pub fn local_ipv4s() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    // SAFETY: standard getifaddrs walk; the list is freed before returning.
    unsafe {
        let mut ifs: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifs) != 0 {
            return out;
        }
        let mut cur = ifs;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null() && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                out.push(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)));
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifs);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{endpoint, is_computer};
    use crate::qr;
    use base64::Engine;

    #[test]
    fn hidden_endpoint_has_no_name() {
        // Captured from a Galaxy A56 in Everyone mode with the screen off.
        assert_eq!(endpoint("MrBfzSJmz0QMn60JuiT_f9k", &qr::tests::key()), (None, false));
    }

    #[test]
    fn hidden_endpoint_with_qr_record() {
        let k = qr::tests::key();
        let mut b = vec![0x32u8]; // hidden phone
        b.extend([0u8; 16]);
        b.extend([2, 1, 1]); // vendor id record, skipped
        let record = qr::tests::encrypt(&k, b"Galaxy A56");
        b.extend([1, record.len() as u8]);
        b.extend(&record);
        let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&b);
        assert_eq!(endpoint(&n, &k), (Some("Galaxy A56".into()), true));
        // Cut short: no panic, no name.
        let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&b[..b.len() - 5]);
        assert_eq!(endpoint(&n, &k), (None, false));
        // Someone else's QR code: not ours, no name.
        let other = qr::tests::encrypt(&qr::tests::other_key(), b"Galaxy A56");
        let mut b = b[..20].to_vec();
        b.extend([1, other.len() as u8]);
        b.extend(&other);
        let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&b);
        assert_eq!(endpoint(&n, &k), (None, false));
    }

    #[test]
    fn skips_computers_only() {
        assert!(!is_computer("MrBfzSJmz0QMn60JuiT_f9k")); // hidden Galaxy A56: phone
        let enc = |f: u8| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([f; 18]);
        assert!(is_computer(&enc(3 << 1)));
        assert!(!is_computer(&enc(1 << 1)));
    }

    #[test]
    fn visible_endpoint_name() {
        let mut b = vec![0x06u8];
        b.extend([0u8; 16]);
        b.push(6);
        b.extend(b"Pixel9");
        let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&b);
        assert_eq!(endpoint(&n, &qr::tests::key()), (Some("Pixel9".into()), false));
        // Visible device that scanned our code: the record is our advertising token.
        let k = qr::tests::key();
        b.extend([1, 16]);
        b.extend(k.token());
        let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&b);
        assert_eq!(endpoint(&n, &k), (Some("Pixel9".into()), true));
    }
}
