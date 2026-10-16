//! Remembered device names, keyed by IP. Hidden Quick Share devices don't advertise a
//! name, but they reveal it when they send to us or are briefly visible.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;

pub struct Names {
    path: PathBuf,
    map: HashMap<Ipv4Addr, String>,
}

impl Names {
    /// File format: one `ip<TAB>name` per line.
    pub fn load(path: PathBuf) -> Self {
        let map = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (ip, name) = l.split_once('\t')?;
                Some((ip.parse().ok()?, name.to_string()))
            })
            .collect();
        Self { path, map }
    }

    pub fn get(&self, ip: &Ipv4Addr) -> Option<&str> {
        self.map.get(ip).map(String::as_str)
    }

    /// Returns true if the name was new or changed.
    pub fn learn(&mut self, ip: Ipv4Addr, name: &str) -> bool {
        let name = name.replace(['\t', '\n'], " ");
        if self.map.get(&ip) == Some(&name) {
            return false;
        }
        self.map.insert(ip, name);
        let body: String = self.map.iter().map(|(ip, n)| format!("{ip}\t{n}\n")).collect();
        if let Err(e) = std::fs::write(&self.path, body) {
            log::warn!("could not save device names: {e}");
        }
        true
    }
}
