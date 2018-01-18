use std::net::SocketAddr;
use std::sync::{RwLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::{HashMap, HashSet};

use proto::{ReceivedImage, AbortedImage};
use cluster::future::UploadOk;
use cluster::error::ErrorKind;
use cluster::config::Config;

use machine_id::MachineId;

#[derive(Debug)]
struct Bookkeeping {
    accepted_ips: HashSet<SocketAddr>,
    done_ips: HashSet<SocketAddr>,
    done_ids: HashSet<MachineId>,
    done_hostnames: HashSet<String>,
    aborted_ips: HashMap<SocketAddr, String>,
    aborted_ids: HashMap<MachineId, String>,
    aborted_hostnames: HashMap<String, String>,
    rejected_ips: HashMap<SocketAddr, String>,
}

/// Current upload statistics
///
/// We're trying to be conservative of what can be published here so that
/// we don't have to break backwards compatibility in the future.
#[derive(Debug)]
pub struct Stats {
    book: RwLock<Bookkeeping>,
    total_responses: AtomicUsize,
}


impl Stats {
    pub(crate) fn new() -> Stats {
        Stats {
            book: RwLock::new(Bookkeeping {
                accepted_ips: HashSet::new(),
                done_ips: HashSet::new(),
                done_ids: HashSet::new(),
                done_hostnames: HashSet::new(),
                aborted_ips: HashMap::new(),
                aborted_ids: HashMap::new(),
                aborted_hostnames: HashMap::new(),
                rejected_ips: HashMap::new(),
            }),
            total_responses: AtomicUsize::new(0),
        }
    }
    pub(crate) fn received_image(&self, addr: SocketAddr, info: &ReceivedImage)
    {
        let mut book = self.book.write()
            .expect("bookkeeping is not poisoned");
        if !info.forwarded {
            book.done_ips.insert(addr);
        }
        book.done_ids.insert(info.machine_id.clone());
        book.done_hostnames.insert(info.hostname.clone());
    }
    pub(crate) fn aborted_image(&self, addr: SocketAddr, info: &AbortedImage) {
        let mut book = self.book.write()
            .expect("bookkeeping is not poisoned");
        if !info.forwarded {
            book.aborted_ips.insert(addr, info.reason.clone());
        }
        book.aborted_ids.insert(
            info.machine_id.clone(), info.reason.clone());
        book.aborted_hostnames.insert(
            info.hostname.clone(), info.reason.clone());
    }
    pub(crate) fn add_response(&self, source: SocketAddr,
        accepted: bool, reject_reason: Option<String>,
        hosts: HashMap<MachineId, String>)
    {
        let mut book = self.book.write()
            .expect("bookkeeping is not poisoned");
        if !accepted {
            warn!("Rejected because of {:?} try {:?}", reject_reason, hosts);
            let res = book.rejected_ips.insert(source,
                reject_reason.unwrap_or_else(|| String::from("unknown")));
            if res.is_none() {
                self.total_responses.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            debug!("Accepted from {}", source);
            let res = book.accepted_ips.insert(source);
            if res {
                self.total_responses.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    pub(crate) fn total_responses(&self) -> u32 {
        self.total_responses.load(Ordering::Relaxed) as u32
    }
}

pub(crate) fn check(stats: &Stats, _config: &Config, _early_timeout: bool)
    -> Option<Result<UploadOk, ErrorKind>>
{
    let book = stats.book.read()
        .expect("bookkeeping is not poisoned");
    // TODO(tailhook) this is very simplistic preliminary check
    if book.accepted_ips.is_superset(&book.done_ips) {
        // TODO(tailhook) check kinds of rejectsion
        if book.rejected_ips.len() > 0 {
            return Some(Err(ErrorKind::Rejected));
        } else {
            return Some(Ok(UploadOk::new()));
        }
    }
    return None;
}
