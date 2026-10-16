//! Find Quick Share receivers on the LAN, including "hidden" ones.
//!
//! Phones in Everyone mode still advertise the hidden form (no name) unless a nearby
//! sender triggers them over BLE, which macOS apps can't do. They accept transfers
//! anyway, so hidden devices are reported too, with `name: None`. Devices are keyed by IP:
//! the Quick Share endpoint id rotates every few minutes, the address doesn't.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use anyhow::Result;
use base64::Engine;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use tokio::sync::mpsc;

const SERVICE: &str = "_FC9F5ED42C8A._tcp.local.";
/// Devices drop and re-add their advertisement when the endpoint id rotates; only report
/// them gone if they stay gone this long, so the AirDrop entry doesn't flicker.
const LOST_GRACE: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum Event {
    Found { ip: Ipv4Addr, name: Option<String>, addr: String },
    Lost { ip: Ipv4Addr },
}

pub fn start() -> Result<mpsc::Receiver<Event>> {
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
                    let name = info.get_property_val_str("n").and_then(device_name);
                    by_ip.entry(ip).or_default().insert(info.get_fullname().to_string());
                    leaving.remove(&ip);
                    Event::Found { ip, name, addr: format!("{ip}:{}", info.get_port()) }
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

/// Device name from the base64url endpoint info, if the device advertises it
/// (byte 0: flags, bytes 1..17: salt + encrypted key, byte 17: name length, then name).
fn device_name(n: &str) -> Option<String> {
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(n.trim_end_matches('=')).ok()?;
    let len = *b.get(17)? as usize;
    let name = String::from_utf8(b.get(18..18 + len)?.to_vec()).ok()?;
    (!name.is_empty()).then_some(name)
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
    use super::device_name;
    use base64::Engine;

    #[test]
    fn hidden_endpoint_has_no_name() {
        // Captured from a Galaxy A56 in Everyone mode with the screen off.
        assert_eq!(device_name("MrBfzSJmz0QMn60JuiT_f9k"), None);
    }

    #[test]
    fn visible_endpoint_name() {
        let mut b = vec![0x06u8];
        b.extend([0u8; 16]);
        b.push(6);
        b.extend(b"Pixel9");
        let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&b);
        assert_eq!(device_name(&n).as_deref(), Some("Pixel9"));
    }
}
