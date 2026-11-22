//! mdns-sd's own responder on UDP 5353. it backs its browse queries off to one an hour, and a
//! receiver whose short-lived records lapse (a TV that misses the refresh queries) is then only
//! found again when someone asks, so a removal and the app's rescan start the browse over.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};

use super::{Found, SERVICE_TYPE, Seen};
use crate::error::{Error, Result};

/// starts the browse over, handing its events to the browser thread.
struct Rescanner {
    daemon: ServiceDaemon,
    browses: Mutex<Sender<Receiver<ServiceEvent>>>,
}

impl Rescanner {
    fn rescan(&self) {
        match self.daemon.browse(&full_type(SERVICE_TYPE)) {
            Ok(events) => {
                let _ = self.browses.lock().expect("airplay browses").send(events);
            }
            Err(e) => tracing::warn!("airplay: mDNS rescan: {e}"),
        }
    }
}

pub(super) struct Browser {
    daemon: ServiceDaemon,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    rescanner: Arc<Rescanner>,
}

impl Browser {
    pub(super) const NAME: &str = "mdns-sd";

    pub(super) fn start(seen: Arc<Seen>) -> Result<Self> {
        let daemon = ServiceDaemon::new().map_err(|e| Error::AirPlay(format!("mDNS: {e}")))?;
        let events = daemon.browse(&full_type(SERVICE_TYPE)).map_err(|e| Error::AirPlay(format!("mDNS browse: {e}")))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let (browses, next_browses) = mpsc::channel();
        let rescanner = Arc::new(Rescanner { daemon: daemon.clone(), browses: Mutex::new(browses) });
        let again = rescanner.clone();
        let thread = thread::Builder::new()
            .name("navia-airplay-mdns".into())
            .spawn(move || {
                let mut events = events;
                while !flag.load(Ordering::Acquire) {
                    match events.recv_timeout(Duration::from_millis(1000)) {
                        Ok(ServiceEvent::ServiceResolved(info)) => {
                            let addresses: Vec<IpAddr> = info.get_addresses().iter().map(|a| a.to_ip_addr()).collect();
                            let txt: HashMap<String, String> = info.txt_properties.iter().map(|p| (p.key().to_string(), p.val_str().to_string())).collect();
                            seen.resolved(Found { key: info.get_fullname().to_string(), name: instance_name(info.get_fullname()), txt, addresses, port: info.get_port() });
                        }
                        Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                            seen.removed(&fullname);
                            if seen.may_rescan("a receiver left") {
                                again.rescan();
                            }
                        }
                        Ok(_) => {}
                        Err(mdns_sd::RecvTimeoutError::Disconnected) if flag.load(Ordering::Acquire) => break,
                        // a rescan's browse replaced this one once its last events were read; the next is queued.
                        Err(mdns_sd::RecvTimeoutError::Disconnected) => match next_browses.recv_timeout(Duration::from_millis(1000)) {
                            Ok(next) => events = next,
                            Err(_) => break,
                        },
                        Err(_) => {}
                    }
                    seen.prune();
                }
            })
            .map_err(Error::Io)?;
        Ok(Self { daemon, stop, thread: Some(thread), rescanner })
    }

    pub(super) fn rescan(&self) {
        self.rescanner.rescan();
    }

    pub(super) fn announce(&self, instance: &str, service_type: &str, host: &str, port: u16, txt: &[(&str, &str)]) -> Result<Announced> {
        let props: HashMap<String, String> = txt.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let info = ServiceInfo::new(&full_type(service_type), instance, &format!("{host}.local."), "", port, props).map_err(|e| Error::AirPlay(format!("mDNS: {e}")))?.enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        self.daemon.register(info).map_err(|e| Error::AirPlay(format!("mDNS register: {e}")))?;
        Ok(Announced { daemon: self.daemon.clone(), fullname })
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.daemon.shutdown();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub(super) struct Announced {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Announced {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
    }
}

fn full_type(service_type: &str) -> String {
    format!("{service_type}.local.")
}

/// "Living Room._airplay._tcp.local." → "Living Room", with mDNS escapes undone.
fn instance_name(fullname: &str) -> String {
    let name = fullname.strip_suffix(&full_type(SERVICE_TYPE)).unwrap_or(fullname).trim_end_matches('.');
    name.replace("\\.", ".").replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(instance_name("[LG] webOS TV UR80006LJ._airplay._tcp.local."), "[LG] webOS TV UR80006LJ");
        assert_eq!(instance_name("Dave\\.s Room._airplay._tcp.local."), "Dave.s Room");
    }
}
