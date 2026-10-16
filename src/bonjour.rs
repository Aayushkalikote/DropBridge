//! Advertise `_airdrop._tcp` services on AWDL through mDNSResponder (dns_sd).
//!
//! Each peer gets its own hostname (an AAAA record pointing at this Mac's awdl0 address):
//! sharingd ignores a peer announced under this Mac's own hostname.

use std::ffi::{c_void, CString};
use std::net::Ipv6Addr;
use std::os::raw::c_char;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{bail, Result};

type DNSServiceRef = *mut c_void;
type DNSRecordRef = *mut c_void;
type RegisterRecordReply =
    extern "C" fn(DNSServiceRef, DNSRecordRef, u32, i32, *mut c_void);

const FLAGS_SHARE_CONNECTION: u32 = 0x4000;
const FLAGS_UNIQUE: u32 = 0x20;
const RR_TYPE_AAAA: u16 = 28;
const RR_CLASS_IN: u16 = 1;

extern "C" {
    fn DNSServiceCreateConnection(sd: *mut DNSServiceRef) -> i32;
    fn DNSServiceRegisterRecord(
        sd: DNSServiceRef,
        rec: *mut DNSRecordRef,
        flags: u32,
        iface: u32,
        fullname: *const c_char,
        rrtype: u16,
        rrclass: u16,
        rdlen: u16,
        rdata: *const c_void,
        ttl: u32,
        cb: RegisterRecordReply,
        ctx: *mut c_void,
    ) -> i32;
    fn DNSServiceUpdateRecord(
        sd: DNSServiceRef,
        rec: DNSRecordRef,
        flags: u32,
        rdlen: u16,
        rdata: *const c_void,
        ttl: u32,
    ) -> i32;
    fn DNSServiceRegister(
        sd: *mut DNSServiceRef,
        flags: u32,
        iface: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        host: *const c_char,
        port_be: u16,
        txtlen: u16,
        txt: *const c_void,
        cb: *const c_void,
        ctx: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(sd: DNSServiceRef) -> i32;
    fn DNSServiceProcessResult(sd: DNSServiceRef) -> i32;
    fn DNSServiceRefDeallocate(sd: DNSServiceRef);
}

extern "C" fn on_record(_: DNSServiceRef, _: DNSRecordRef, _: u32, err: i32, _: *mut c_void) {
    if err != 0 {
        log::warn!("mDNS host record registration failed: {err}");
    }
}

/// A registered AirDrop service. Unregisters (with mDNS goodbyes) on drop.
pub struct Advertisement {
    stop: Arc<AtomicBool>,
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Register `instance._airdrop._tcp` on AWDL, served by `host.local.` at `port`.
pub fn advertise(instance: &str, host: &str, port: u16, flags: u32) -> Result<Advertisement> {
    let addr = awdl_address().ok_or_else(|| anyhow::anyhow!("awdl0 has no IPv6 address"))?;
    let instance = CString::new(instance)?;
    let fullhost = CString::new(format!("{host}.local."))?;
    let regtype = CString::new("_airdrop._tcp")?;
    let txt_entry = format!("flags={flags}");
    let mut txt = vec![txt_entry.len() as u8];
    txt.extend_from_slice(txt_entry.as_bytes());

    // Explicit awdl0 index (not kDNSServiceInterfaceIndexP2P): answers then carry the
    // interface scope that a link-local address needs.
    // SAFETY: if_nametoindex takes a NUL-terminated name.
    let iface = unsafe { libc::if_nametoindex(c"awdl0".as_ptr()) };
    if iface == 0 {
        bail!("no awdl0 interface");
    }
    let mut conn: DNSServiceRef = ptr::null_mut();
    let mut record: DNSRecordRef = ptr::null_mut();
    let mut service: DNSServiceRef;
    // SAFETY: plain dns_sd calls; every pointer outlives the call, and `conn` (which owns
    // the record and service) is only touched by the thread below after this point.
    unsafe {
        check(DNSServiceCreateConnection(&mut conn), "DNSServiceCreateConnection")?;
        let octets = addr.octets();
        let err = DNSServiceRegisterRecord(
            conn, &mut record, FLAGS_UNIQUE, iface, fullhost.as_ptr(),
            RR_TYPE_AAAA, RR_CLASS_IN, 16, octets.as_ptr().cast(), 120, on_record, ptr::null_mut(),
        );
        if err != 0 {
            DNSServiceRefDeallocate(conn);
            bail!("DNSServiceRegisterRecord failed: {err}");
        }
        service = conn;
        let err = DNSServiceRegister(
            &mut service, FLAGS_SHARE_CONNECTION, iface, instance.as_ptr(), regtype.as_ptr(),
            ptr::null(), fullhost.as_ptr(), port.to_be(), txt.len() as u16, txt.as_ptr().cast(),
            ptr::null(), ptr::null_mut(),
        );
        if err != 0 {
            DNSServiceRefDeallocate(conn);
            bail!("DNSServiceRegister failed: {err}");
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let (conn, record) = (conn as usize, record as usize); // raw pointers aren't Send
    thread::spawn(move || {
        let (conn, record) = (conn as DNSServiceRef, record as DNSRecordRef);
        let mut current = addr;
        // SAFETY: this thread is now the only user of `conn` and `record`.
        unsafe {
            let mut pfd = libc::pollfd { fd: DNSServiceRefSockFD(conn), events: libc::POLLIN, revents: 0 };
            let mut ticks = 0u32;
            while !stop_thread.load(Ordering::Relaxed) {
                if libc::poll(&mut pfd, 1, 250) > 0 && DNSServiceProcessResult(conn) != 0 {
                    log::warn!("mDNSResponder connection lost");
                    break;
                }
                ticks += 1;
                // awdl0 rotates its MAC (and so its link-local address); keep the AAAA current.
                if ticks % 20 == 0 {
                    if let Some(a) = awdl_address().filter(|a| *a != current) {
                        current = a;
                        let o = a.octets();
                        DNSServiceUpdateRecord(conn, record, 0, 16, o.as_ptr().cast(), 120);
                    }
                }
            }
            DNSServiceRefDeallocate(conn); // also drops the service and sends goodbyes
        }
    });
    Ok(Advertisement { stop })
}

fn check(err: i32, what: &str) -> Result<()> {
    if err != 0 {
        bail!("{what} failed: {err}");
    }
    Ok(())
}

/// This Mac's link-local IPv6 address on awdl0.
pub fn awdl_address() -> Option<Ipv6Addr> {
    let mut found = None;
    // SAFETY: standard getifaddrs walk; the list is freed before returning.
    unsafe {
        let mut ifs: *mut libc::ifaddrs = ptr::null_mut();
        if libc::getifaddrs(&mut ifs) != 0 {
            return None;
        }
        let mut cur = ifs;
        while !cur.is_null() {
            let ifa = &*cur;
            let name = std::ffi::CStr::from_ptr(ifa.ifa_name);
            if name.to_bytes() == b"awdl0"
                && !ifa.ifa_addr.is_null()
                && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET6
            {
                let sin6 = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                let mut octets = sin6.sin6_addr.s6_addr;
                // KAME-style embedded scope id in bytes 2..4 must be cleared.
                octets[2] = 0;
                octets[3] = 0;
                let a = Ipv6Addr::from(octets);
                if a.segments()[0] & 0xffc0 == 0xfe80 {
                    found = Some(a);
                    break;
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifs);
    }
    found
}
