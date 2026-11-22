//! finds AirPlay receivers. on the Mac through the system's Bonjour (mDNSResponder owns UDP 5353
//! there and answers for every app); elsewhere with mdns-sd in pure
//! Rust (no Avahi or Bonjour library, so Windows needs nothing installed). receivers that sleep
//! drop off and come back often, so a removal only counts after a grace period.

#[cfg(target_os = "macos")]
mod bonjour;
#[cfg(not(target_os = "macos"))]
mod mdns;

#[cfg(target_os = "macos")]
use bonjour::{Announced, Browser};
#[cfg(not(target_os = "macos"))]
use mdns::{Announced, Browser};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::device::AirPlayDevice;
use crate::error::Result;

const SERVICE_TYPE: &str = "_airplay._tcp";
/// how long a receiver stays listed after the network says it left.
const REMOVE_AFTER: Duration = Duration::from_secs(30);
/// rescans closer together than this are dropped; a new browse already re-queries for a while.
const RESCAN_GAP: Duration = Duration::from_secs(5);
/// how often this machine's own addresses are read again, to hide its own receiver.
const OWN_ADDRESSES_TTL: Duration = Duration::from_secs(60);

/// a receiver as a browser resolved it.
struct Found {
    /// the browser's key for it, the same in a later removal.
    key: String,
    name: String,
    txt: HashMap<String, String>,
    addresses: Vec<IpAddr>,
    port: u16,
}

struct Entry {
    device: AirPlayDevice,
    gone_since: Option<Instant>,
}

/// what the browser thread reports into and the app reads.
struct Seen {
    entries: Mutex<BTreeMap<String, Entry>>,
    on_change: Box<dyn Fn() + Send + Sync>,
    own: Mutex<(HashSet<IpAddr>, Instant)>,
    last_rescan: Mutex<Option<Instant>>,
}

impl Seen {
    fn resolved(&self, f: Found) {
        let own = {
            let mut own = self.own.lock().expect("own addresses");
            if own.1.elapsed() > OWN_ADDRESSES_TTL {
                *own = (local_addresses(), Instant::now());
            }
            f.addresses.iter().any(|a| own.0.contains(a))
        };
        if own {
            return;
        }
        let Some(device) = AirPlayDevice::from_txt(&f.name, &f.txt, sort_addresses(f.addresses), f.port) else { return };
        {
            let mut m = self.entries.lock().expect("airplay devices");
            let known = m.get(&f.key).filter(|e| e.gone_since.is_none()).map(|e| (e.device.addresses.clone(), e.device.port));
            if known.as_ref() != Some(&(device.addresses.clone(), device.port)) {
                tracing::info!("airplay: found {} ({}) at {:?} port {}", device.name, device.model, device.addresses, device.port);
            }
            m.insert(f.key, Entry { device, gone_since: None });
        }
        (self.on_change)();
    }

    fn removed(&self, key: &str) {
        if let Some(e) = self.entries.lock().expect("airplay devices").get_mut(key)
            && e.gone_since.is_none()
        {
            tracing::info!("airplay: {} left the network", e.device.name);
            e.gone_since = Some(Instant::now());
        }
    }

    /// drops receivers gone longer than the grace period; browsers call it about once a second.
    fn prune(&self) {
        let pruned = {
            let mut m = self.entries.lock().expect("airplay devices");
            let before = m.len();
            m.retain(|_, e| {
                let keep = e.gone_since.is_none_or(|t| t.elapsed() < REMOVE_AFTER);
                if !keep {
                    tracing::info!("airplay: {} no longer listed", e.device.name);
                }
                keep
            });
            m.len() != before
        };
        if pruned {
            (self.on_change)();
        }
    }

    /// whether a rescan may run now, and notes that it does.
    fn may_rescan(&self, why: &str) -> bool {
        let mut last = self.last_rescan.lock().expect("airplay rescan");
        if last.is_some_and(|t| t.elapsed() < RESCAN_GAP) {
            return false;
        }
        *last = Some(Instant::now());
        tracing::info!("airplay: searching again ({why})");
        true
    }
}

/// a service this app announces (the remote-control one); dropping it withdraws it.
pub struct Announcement {
    _announced: Announced,
}

pub struct Discovery {
    seen: Arc<Seen>,
    browser: Browser,
}

impl Discovery {
    /// start browsing. `on_change` runs on the browser's thread whenever the list changes.
    pub fn start(on_change: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let seen = Arc::new(Seen { entries: Mutex::new(BTreeMap::new()), on_change: Box::new(on_change), own: Mutex::new((local_addresses(), Instant::now())), last_rescan: Mutex::new(None) });
        let browser = Browser::start(seen.clone())?;
        tracing::info!("airplay: looking for receivers with {}", Browser::NAME);
        Ok(Self { seen, browser })
    }

    /// receivers currently on the network, by name.
    pub fn devices(&self) -> Vec<AirPlayDevice> {
        let m = self.seen.entries.lock().expect("airplay devices");
        let mut out: Vec<AirPlayDevice> = m.values().map(|e| e.device.clone()).collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out.dedup_by(|a, b| a.id == b.id);
        out
    }

    pub fn find(&self, id: &str) -> Option<AirPlayDevice> {
        self.devices().into_iter().find(|d| d.id.eq_ignore_ascii_case(id))
    }

    /// ask the network again now (a list about to be shown, this machine woke).
    pub fn rescan(&self) {
        if self.seen.may_rescan("asked") {
            self.browser.rescan();
        }
    }

    /// announce `instance` of `service_type` (such as "_dacp._tcp") on `port`. `host` names this
    /// machine where the announcer has no name of its own (mdns-sd); Bonjour uses the system's.
    pub fn announce(&self, instance: &str, service_type: &str, host: &str, port: u16, txt: &[(&str, &str)]) -> Result<Announcement> {
        Ok(Announcement { _announced: self.browser.announce(instance, service_type, host, port, txt)? })
    }
}

/// IPv4 first, then global IPv6, then link-local.
fn sort_addresses(mut a: Vec<IpAddr>) -> Vec<IpAddr> {
    a.sort_by_key(|ip| {
        let class = match ip {
            IpAddr::V4(_) => 0,
            IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80 => 2,
            IpAddr::V6(_) => 1,
        };
        (class, *ip)
    });
    a.dedup();
    a
}

fn local_addresses() -> HashSet<IpAddr> {
    if_addrs::get_if_addrs().map(|v| v.into_iter().map(|i| i.ip()).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_order() {
        let sorted = sort_addresses(vec!["fe80::1".parse().unwrap(), "2001:db8::1".parse().unwrap(), "192.168.1.2".parse().unwrap()]);
        assert_eq!(sorted[0], "192.168.1.2".parse::<IpAddr>().unwrap());
        assert_eq!(sorted[2], "fe80::1".parse::<IpAddr>().unwrap());
    }
}
