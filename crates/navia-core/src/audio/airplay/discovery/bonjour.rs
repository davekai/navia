//! the system's Bonjour through `dns_sd.h` (in libSystem). every request shares one connection to
//! mDNSResponder, owned by one thread, which is the only one that touches the refs.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{Found, SERVICE_TYPE, Seen};
use crate::error::{Error, Result};

type ServiceRef = *mut c_void;
type Flags = u32;
type ErrorCode = i32;

const FLAG_MORE_COMING: Flags = 0x1;
const FLAG_ADD: Flags = 0x2;
const FLAG_SHARE_CONNECTION: Flags = 0x4000;
const PROTOCOL_IPV4_IPV6: u32 = 0x3;
const INTERFACE_ANY: u32 = 0;
const NO_ERROR: ErrorCode = 0;
const ERR_POLICY_DENIED: ErrorCode = -65570;

/// how long the thread waits for mDNSResponder before it looks at commands and prunes.
const TICK: Duration = Duration::from_millis(250);
/// before trying mDNSResponder again after the connection broke.
const RECONNECT_AFTER: Duration = Duration::from_secs(1);
/// a receiver the new browse hasn't listed again this long after a rescan has left.
const RESCAN_CONFIRM: Duration = Duration::from_secs(10);

type BrowseReply = extern "C" fn(ServiceRef, Flags, u32, ErrorCode, *const c_char, *const c_char, *const c_char, *mut c_void);
type ResolveReply = extern "C" fn(ServiceRef, Flags, u32, ErrorCode, *const c_char, *const c_char, u16, u16, *const u8, *mut c_void);
type AddrInfoReply = extern "C" fn(ServiceRef, Flags, u32, ErrorCode, *const c_char, *const libc::sockaddr, u32, *mut c_void);
type RegisterReply = extern "C" fn(ServiceRef, Flags, ErrorCode, *const c_char, *const c_char, *const c_char, *mut c_void);

unsafe extern "C" {
    fn DNSServiceCreateConnection(sd: *mut ServiceRef) -> ErrorCode;
    fn DNSServiceRefSockFD(sd: ServiceRef) -> c_int;
    fn DNSServiceProcessResult(sd: ServiceRef) -> ErrorCode;
    fn DNSServiceRefDeallocate(sd: ServiceRef);
    fn DNSServiceBrowse(sd: *mut ServiceRef, flags: Flags, interface: u32, regtype: *const c_char, domain: *const c_char, reply: BrowseReply, context: *mut c_void) -> ErrorCode;
    fn DNSServiceResolve(sd: *mut ServiceRef, flags: Flags, interface: u32, name: *const c_char, regtype: *const c_char, domain: *const c_char, reply: ResolveReply, context: *mut c_void) -> ErrorCode;
    fn DNSServiceGetAddrInfo(sd: *mut ServiceRef, flags: Flags, interface: u32, protocol: u32, hostname: *const c_char, reply: AddrInfoReply, context: *mut c_void) -> ErrorCode;
    fn DNSServiceRegister(sd: *mut ServiceRef, flags: Flags, interface: u32, name: *const c_char, regtype: *const c_char, domain: *const c_char, host: *const c_char, port: u16, txt_len: u16, txt: *const c_void, reply: RegisterReply, context: *mut c_void) -> ErrorCode;
}

enum Command {
    Rescan,
    Announce { id: u64, spec: Spec, done: Sender<Result<()>> },
    Withdraw(u64),
}

#[derive(Clone)]
struct Spec {
    instance: CString,
    service_type: CString,
    port: u16,
    txt: Vec<u8>,
}

pub(super) struct Browser {
    commands: Sender<Command>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl Browser {
    pub(super) const NAME: &str = "Bonjour";

    pub(super) fn start(seen: Arc<Seen>) -> Result<Self> {
        let (commands, inbox) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::Builder::new()
            .name("navia-airplay-bonjour".into())
            .spawn(move || run(seen, inbox, flag, ready_tx))
            .map_err(Error::Io)?;
        match ready.recv() {
            Ok(Ok(())) => Ok(Self { commands, stop, thread: Some(thread), next_id: 1.into() }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(Error::AirPlay("Bonjour browser stopped".into())),
        }
    }

    pub(super) fn rescan(&self) {
        let _ = self.commands.send(Command::Rescan);
    }

    pub(super) fn announce(&self, instance: &str, service_type: &str, _host: &str, port: u16, txt: &[(&str, &str)]) -> Result<Announced> {
        let cstr = |s: &str| CString::new(s).map_err(|_| Error::AirPlay(format!("Bonjour: bad name {s:?}")));
        let spec = Spec { instance: cstr(instance)?, service_type: cstr(service_type)?, port, txt: txt_record(txt) };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (done, answer) = mpsc::channel();
        self.commands.send(Command::Announce { id, spec, done }).map_err(|_| Error::AirPlay("Bonjour browser stopped".into()))?;
        answer.recv().map_err(|_| Error::AirPlay("Bonjour browser stopped".into()))??;
        Ok(Announced { commands: self.commands.clone(), id })
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub(super) struct Announced {
    commands: Sender<Command>,
    id: u64,
}

impl Drop for Announced {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Withdraw(self.id));
    }
}

/// one receiver being resolved. boxed, so its address is the callbacks' context.
struct Instance {
    state: *mut State,
    key: String,
    name: String,
    /// interfaces Bonjour sees it on; it left when none are.
    interfaces: Vec<u32>,
    /// listed by the current browse (a rescan clears it until the new browse lists it again).
    confirmed: bool,
    resolve: ServiceRef,
    addr_info: ServiceRef,
    host: String,
    port: u16,
    txt: HashMap<String, String>,
    addresses: Vec<IpAddr>,
}

struct State {
    seen: Arc<Seen>,
    connection: ServiceRef,
    browse: ServiceRef,
    instances: HashMap<String, Box<Instance>>,
    /// when to drop the instances a rescan's browse didn't list again.
    sweep_at: Option<Instant>,
    /// announcements by id, with their ref while registered.
    announced: HashMap<u64, (Spec, ServiceRef)>,
    regtype: CString,
}

fn run(seen: Arc<Seen>, inbox: Receiver<Command>, stop: Arc<AtomicBool>, ready: Sender<Result<()>>) {
    let mut state = Box::new(State { seen, connection: ptr::null_mut(), browse: ptr::null_mut(), instances: HashMap::new(), sweep_at: None, announced: HashMap::new(), regtype: CString::new(SERVICE_TYPE).expect("service type") });
    let state_ptr: *mut State = &mut *state;
    let first = unsafe { connect(state_ptr) };
    let started = first.is_ok();
    let _ = ready.send(first);
    if !started {
        return;
    }
    while !stop.load(Ordering::Acquire) {
        unsafe {
            if (*state_ptr).connection.is_null() {
                thread::sleep(RECONNECT_AFTER);
                if let Err(e) = connect(state_ptr) {
                    tracing::warn!("airplay: Bonjour: {e}");
                }
            } else if readable((*state_ptr).connection) {
                let err = DNSServiceProcessResult((*state_ptr).connection);
                if err != NO_ERROR {
                    tracing::warn!("airplay: Bonjour connection broke ({err}), reconnecting");
                    disconnect(state_ptr);
                }
            }
            while let Ok(cmd) = inbox.try_recv() {
                handle(state_ptr, cmd);
            }
            if (*state_ptr).sweep_at.is_some_and(|t| Instant::now() >= t) {
                sweep(state_ptr);
            }
            (*state_ptr).seen.prune();
        }
    }
    unsafe { disconnect(state_ptr) };
}

/// waits up to `TICK` for mDNSResponder to have something.
unsafe fn readable(connection: ServiceRef) -> bool {
    let fd = unsafe { DNSServiceRefSockFD(connection) };
    if fd < 0 {
        return false;
    }
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let n = unsafe { libc::poll(&mut p, 1, TICK.as_millis() as c_int) };
    n > 0 && p.revents != 0
}

/// opens the connection, starts the browse and announces again what was announced.
unsafe fn connect(state: *mut State) -> Result<()> {
    let s = unsafe { &mut *state };
    let mut connection: ServiceRef = ptr::null_mut();
    let err = unsafe { DNSServiceCreateConnection(&mut connection) };
    if err != NO_ERROR {
        return Err(Error::AirPlay(format!("Bonjour isn't available ({err})")));
    }
    s.connection = connection;
    if let Err(e) = unsafe { browse(state) } {
        unsafe { disconnect(state) };
        return Err(e);
    }
    let ids: Vec<u64> = s.announced.keys().copied().collect();
    for id in ids {
        if let Err(e) = unsafe { register(state, id) } {
            tracing::warn!("airplay: Bonjour announce: {e}");
        }
    }
    Ok(())
}

/// ends every request; deallocating the shared connection frees the refs made on it.
unsafe fn disconnect(state: *mut State) {
    let s = unsafe { &mut *state };
    if !s.connection.is_null() {
        unsafe { DNSServiceRefDeallocate(s.connection) };
    }
    s.connection = ptr::null_mut();
    s.browse = ptr::null_mut();
    // listed again once resolved on the new connection, within the grace period.
    for key in s.instances.keys() {
        s.seen.removed(key);
    }
    s.instances.clear();
    s.sweep_at = None;
    for (_, r) in s.announced.values_mut() {
        *r = ptr::null_mut();
    }
}

unsafe fn browse(state: *mut State) -> Result<()> {
    let s = unsafe { &mut *state };
    let mut r = s.connection;
    let err = unsafe { DNSServiceBrowse(&mut r, FLAG_SHARE_CONNECTION, INTERFACE_ANY, s.regtype.as_ptr(), ptr::null(), on_browse, state.cast()) };
    if err != NO_ERROR {
        return Err(Error::AirPlay(format!("Bonjour browse failed ({err}){}", denied_hint(err))));
    }
    s.browse = r;
    Ok(())
}

unsafe fn handle(state: *mut State, cmd: Command) {
    let s = unsafe { &mut *state };
    match cmd {
        Command::Rescan => {
            if s.connection.is_null() {
                return;
            }
            // a new browse question makes mDNSResponder query the network again; the resolves
            // stay, and a receiver that's gone is removed through the new browse.
            for i in s.instances.values_mut() {
                i.confirmed = false;
            }
            s.sweep_at = Some(Instant::now() + RESCAN_CONFIRM);
            unsafe {
                if !s.browse.is_null() {
                    DNSServiceRefDeallocate(s.browse);
                    s.browse = ptr::null_mut();
                }
                if let Err(e) = browse(state) {
                    tracing::warn!("airplay: {e}");
                }
            }
        }
        Command::Announce { id, spec, done } => {
            s.announced.insert(id, (spec, ptr::null_mut()));
            let result = if s.connection.is_null() { Ok(()) } else { unsafe { register(state, id) } };
            if result.is_err() {
                s.announced.remove(&id);
            }
            let _ = done.send(result);
        }
        Command::Withdraw(id) => {
            if let Some((_, r)) = s.announced.remove(&id)
                && !r.is_null()
            {
                unsafe { DNSServiceRefDeallocate(r) };
            }
        }
    }
}

unsafe fn sweep(state: *mut State) {
    let s = unsafe { &mut *state };
    s.sweep_at = None;
    let gone: Vec<String> = s.instances.iter().filter(|(_, i)| !i.confirmed).map(|(k, _)| k.clone()).collect();
    for key in gone {
        if let Some(i) = s.instances.remove(&key) {
            unsafe { end_instance(&i) };
            s.seen.removed(&key);
        }
    }
}

unsafe fn register(state: *mut State, id: u64) -> Result<()> {
    let s = unsafe { &mut *state };
    let Some((spec, slot)) = s.announced.get_mut(&id) else { return Ok(()) };
    let mut r = s.connection;
    let err = unsafe {
        DNSServiceRegister(&mut r, FLAG_SHARE_CONNECTION, INTERFACE_ANY, spec.instance.as_ptr(), spec.service_type.as_ptr(), ptr::null(), ptr::null(), spec.port.to_be(), spec.txt.len() as u16, spec.txt.as_ptr().cast(), on_register, ptr::null_mut())
    };
    if err != NO_ERROR {
        return Err(Error::AirPlay(format!("Bonjour register failed ({err}){}", denied_hint(err))));
    }
    *slot = r;
    Ok(())
}

unsafe fn end_instance(i: &Instance) {
    unsafe {
        if !i.addr_info.is_null() {
            DNSServiceRefDeallocate(i.addr_info);
        }
        if !i.resolve.is_null() {
            DNSServiceRefDeallocate(i.resolve);
        }
    }
}

fn denied_hint(err: ErrorCode) -> &'static str {
    if err == ERR_POLICY_DENIED { ": Local Network access is off for this app" } else { "" }
}

unsafe fn text(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

extern "C" fn on_browse(_sd: ServiceRef, flags: Flags, interface: u32, err: ErrorCode, name: *const c_char, regtype: *const c_char, domain: *const c_char, context: *mut c_void) {
    let state: *mut State = context.cast();
    let s = unsafe { &mut *state };
    if err != NO_ERROR {
        tracing::warn!("airplay: Bonjour browse error {err}{}", denied_hint(err));
        return;
    }
    let (name_s, regtype_s, domain_s) = unsafe { (text(name), text(regtype), text(domain)) };
    let key = format!("{name_s}.{regtype_s}{domain_s}");
    if flags & FLAG_ADD != 0 {
        if let Some(i) = s.instances.get_mut(&key) {
            if !i.interfaces.contains(&interface) {
                i.interfaces.push(interface);
            }
            i.confirmed = true;
            return;
        }
        let mut instance = Box::new(Instance { state, key: key.clone(), name: name_s, interfaces: vec![interface], confirmed: true, resolve: ptr::null_mut(), addr_info: ptr::null_mut(), host: String::new(), port: 0, txt: HashMap::new(), addresses: Vec::new() });
        let ctx: *mut Instance = &mut *instance;
        let mut r = s.connection;
        let e = unsafe { DNSServiceResolve(&mut r, FLAG_SHARE_CONNECTION, interface, name, regtype, domain, on_resolve, ctx.cast()) };
        if e != NO_ERROR {
            tracing::warn!("airplay: Bonjour resolve of {} failed ({e})", instance.name);
            return;
        }
        instance.resolve = r;
        s.instances.insert(key, instance);
    } else if let Some(i) = s.instances.get_mut(&key) {
        i.interfaces.retain(|x| *x != interface);
        if i.interfaces.is_empty() {
            let i = s.instances.remove(&key).expect("instance");
            unsafe { end_instance(&i) };
            s.seen.removed(&key);
        }
    }
}

extern "C" fn on_resolve(_sd: ServiceRef, _flags: Flags, interface: u32, err: ErrorCode, _fullname: *const c_char, host: *const c_char, port: u16, txt_len: u16, txt: *const u8, context: *mut c_void) {
    let i = unsafe { &mut *context.cast::<Instance>() };
    if err != NO_ERROR {
        tracing::warn!("airplay: Bonjour resolve of {} error {err}", i.name);
        return;
    }
    let bytes = if txt.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(txt, txt_len as usize) } };
    i.txt = parse_txt(bytes);
    i.port = u16::from_be(port);
    let host = unsafe { text(host) };
    if host != i.host || i.addr_info.is_null() {
        i.host = host;
        i.addresses.clear();
        let s = unsafe { &mut *i.state };
        unsafe {
            if !i.addr_info.is_null() {
                DNSServiceRefDeallocate(i.addr_info);
                i.addr_info = ptr::null_mut();
            }
            let Ok(name) = CString::new(i.host.clone()) else { return };
            let mut r = s.connection;
            let ctx: *mut Instance = i;
            let e = DNSServiceGetAddrInfo(&mut r, FLAG_SHARE_CONNECTION, interface, PROTOCOL_IPV4_IPV6, name.as_ptr(), on_addr_info, ctx.cast());
            if e != NO_ERROR {
                tracing::warn!("airplay: Bonjour address lookup of {} failed ({e})", i.host);
                return;
            }
            i.addr_info = r;
        }
    } else {
        report(i);
    }
}

extern "C" fn on_addr_info(_sd: ServiceRef, flags: Flags, _interface: u32, err: ErrorCode, _host: *const c_char, address: *const libc::sockaddr, _ttl: u32, context: *mut c_void) {
    let i = unsafe { &mut *context.cast::<Instance>() };
    if err != NO_ERROR {
        tracing::warn!("airplay: Bonjour address of {} error {err}", i.name);
        return;
    }
    if let Some(ip) = unsafe { ip_of(address) } {
        if flags & FLAG_ADD != 0 {
            if !i.addresses.contains(&ip) {
                i.addresses.push(ip);
            }
        } else {
            i.addresses.retain(|a| *a != ip);
        }
    }
    if flags & FLAG_MORE_COMING == 0 {
        report(i);
    }
}

fn report(i: &Instance) {
    if i.addresses.is_empty() || i.port == 0 {
        return;
    }
    let s = unsafe { &*i.state };
    s.seen.resolved(Found { key: i.key.clone(), name: i.name.clone(), txt: i.txt.clone(), addresses: i.addresses.clone(), port: i.port });
}

extern "C" fn on_register(_sd: ServiceRef, _flags: Flags, err: ErrorCode, name: *const c_char, regtype: *const c_char, _domain: *const c_char, _context: *mut c_void) {
    if err != NO_ERROR {
        let (name, regtype) = unsafe { (text(name), text(regtype)) };
        tracing::warn!("airplay: Bonjour announce of {name} {regtype} error {err}{}", denied_hint(err));
    }
}

unsafe fn ip_of(address: *const libc::sockaddr) -> Option<IpAddr> {
    if address.is_null() {
        return None;
    }
    match unsafe { (*address).sa_family } as c_int {
        libc::AF_INET => {
            let a = unsafe { &*address.cast::<libc::sockaddr_in>() };
            Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr))))
        }
        libc::AF_INET6 => {
            let a = unsafe { &*address.cast::<libc::sockaddr_in6>() };
            Some(IpAddr::V6(Ipv6Addr::from(a.sin6_addr.s6_addr)))
        }
        _ => None,
    }
}

/// DNS TXT: length-prefixed `key=value` strings.
fn parse_txt(mut bytes: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    while let Some((&len, rest)) = bytes.split_first() {
        let len = (len as usize).min(rest.len());
        let (item, next) = rest.split_at(len);
        bytes = next;
        let item = String::from_utf8_lossy(item);
        match item.split_once('=') {
            Some((k, v)) if !k.is_empty() => out.insert(k.to_string(), v.to_string()),
            _ if !item.is_empty() => out.insert(item.to_string(), String::new()),
            _ => None,
        };
    }
    out
}

fn txt_record(items: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in items {
        let item = format!("{k}={v}");
        let bytes = &item.as_bytes()[..item.len().min(u8::MAX as usize)];
        out.push(bytes.len() as u8);
        out.extend_from_slice(bytes);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_round_trip() {
        let rec = txt_record(&[("txtvers", "1"), ("DbId", "ABC")]);
        let parsed = parse_txt(&rec);
        assert_eq!(parsed.get("txtvers").map(String::as_str), Some("1"));
        assert_eq!(parsed.get("DbId").map(String::as_str), Some("ABC"));
        assert!(parse_txt(&[5, b'a', b'=']).get("a").is_some());
    }
}
