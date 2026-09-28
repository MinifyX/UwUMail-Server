//! A small TTL cache for the DNS answers mail-auth asks for.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mail_auth::common::parse::TxtRecordParser;
use mail_auth::common::verify::DomainKey;
use mail_auth::dmarc::Dmarc;
use mail_auth::hickory_resolver::proto::op::ResponseCode;
use mail_auth::mta_sts::TlsRpt;
use mail_auth::spf::Spf;
use mail_auth::{DnsError, DnssecStatus, MX, Parameters, RecordSet, ResolverCache, Txt};

use crate::dane::{HostTlsa, Tlsa, ValidatedMx, tlsa_name};

const CAPACITY: usize = 10_000;

pub struct TtlCache<K, V> {
    entries: Mutex<HashMap<K, (V, Instant)>>,
}

impl<K, V> Default for TtlCache<K, V> {
    fn default() -> Self {
        TtlCache { entries: Mutex::new(HashMap::new()) }
    }
}

impl<K: Hash + Eq, V: Clone> ResolverCache<K, V> for TtlCache<K, V> {
    fn get<Q>(&self, name: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut entries = self.entries.lock().expect("dns cache poisoned");
        match entries.get(name) {
            Some((value, valid_until)) if *valid_until > Instant::now() => Some(value.clone()),
            Some(_) => {
                entries.remove(name);
                None
            }
            None => None,
        }
    }

    fn remove<Q>(&self, name: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entries.lock().expect("dns cache poisoned").remove(name).map(|(value, _)| value)
    }

    fn insert(&self, key: K, value: V, valid_until: Instant) {
        let mut entries = self.entries.lock().expect("dns cache poisoned");
        if entries.len() >= CAPACITY {
            let now = Instant::now();
            entries.retain(|_, (_, until)| *until > now);
            if entries.len() >= CAPACITY {
                entries.clear();
            }
        }
        entries.insert(key, (value, valid_until));
    }
}

type TxtCache = TtlCache<Box<str>, Txt>;
type MxCache = TtlCache<Box<str>, RecordSet<MX>>;
type Ipv4Cache = TtlCache<Box<str>, RecordSet<Ipv4Addr>>;
type Ipv6Cache = TtlCache<Box<str>, RecordSet<Ipv6Addr>>;
type PtrCache = TtlCache<IpAddr, RecordSet<Box<str>>>;

pub type CachedParameters<'x, P> = Parameters<'x, P, TxtCache, MxCache, Ipv4Cache, Ipv6Cache, PtrCache>;

#[derive(Default)]
pub struct DnsCaches {
    pub(crate) txt: TxtCache,
    pub(crate) mx: MxCache,
    pub(crate) ipv4: Ipv4Cache,
    pub(crate) ipv6: Ipv6Cache,
    pub(crate) ptr: PtrCache,
    /// A domain's MX records as the DNSSEC-validating resolver answers, for DANE.
    pub(crate) dane_mx: TtlCache<Box<str>, ValidatedMx>,
    /// The validated TLSA records at `_25._tcp.<host>`.
    pub(crate) tlsa: TtlCache<Box<str>, HostTlsa>,
}

fn fqdn(name: &str) -> Box<str> {
    let mut name = name.to_ascii_lowercase();
    if !name.ends_with('.') {
        name.push('.');
    }
    name.into_boxed_str()
}

fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(365 * 24 * 3600)
}

impl DnsCaches {
    pub(crate) fn params<P>(&self, params: P) -> CachedParameters<'_, P> {
        Parameters::new(params)
            .with_txt_cache(&self.txt)
            .with_mx_cache(&self.mx)
            .with_ptr_cache(&self.ptr)
            .with_ipv4_cache(&self.ipv4)
            .with_ipv6_cache(&self.ipv6)
    }

    /// Pins a TXT record (DKIM key, SPF or DMARC policy), e.g. for tests.
    pub fn pin_txt(&self, name: &str, record: &str) -> Result<(), String> {
        let bytes = record.as_bytes();
        let txt = if record.starts_with("v=spf1") {
            Txt::Spf(Arc::new(Spf::parse(bytes).map_err(|e| e.to_string())?))
        } else if record.starts_with("v=DMARC1") {
            Txt::Dmarc(Arc::new(Dmarc::parse(bytes).map_err(|e| e.to_string())?))
        } else if record.starts_with("v=TLSRPTv1") {
            Txt::TlsRpt(Arc::new(TlsRpt::parse(bytes).map_err(|e| e.to_string())?))
        } else {
            Txt::DomainKey(Arc::new(DomainKey::parse(bytes).map_err(|e| e.to_string())?))
        };
        self.txt.insert(fqdn(name), txt, far_future());
        Ok(())
    }

    /// Pins "no such record" for a name, so lookups do not leave the machine.
    pub fn pin_no_txt(&self, name: &str) {
        let error = mail_auth::Error::Dns(DnsError::RecordNotFound(ResponseCode::NXDomain));
        self.txt.insert(fqdn(name), Txt::Error(error), far_future());
    }

    /// Pins MX records that are not signed, so DANE does not apply.
    pub fn pin_mx(&self, domain: &str, exchanges: &[(u16, &str)]) {
        self.pin_unvalidated_mx(domain, exchanges);
        self.dane_mx.insert(fqdn(domain), ValidatedMx::Insecure, far_future());
    }

    /// Pins what the ordinary, non-validating resolver answers for a domain's MX records (none:
    /// "no MX"), and leaves what the validating one says alone: a forged answer, in tests.
    pub fn pin_unvalidated_mx(&self, domain: &str, exchanges: &[(u16, &str)]) {
        let records: Arc<[MX]> = exchanges
            .iter()
            .map(|(preference, host)| MX { preference: *preference, exchanges: vec![fqdn(host)].into_boxed_slice() })
            .collect();
        self.mx.insert(
            fqdn(domain),
            RecordSet { rrset: records, dnssec_status: DnssecStatus::Indeterminate },
            far_future(),
        );
    }

    /// Pins MX records that validate with DNSSEC, so the TLSA records of their hosts count.
    pub fn pin_signed_mx(&self, domain: &str, exchanges: &[(u16, &str)]) {
        self.pin_unvalidated_mx(domain, exchanges);
        let mut sorted = exchanges.to_vec();
        sorted.sort_by_key(|(preference, _)| *preference);
        let hosts = sorted.iter().map(|(_, host)| host.trim_end_matches('.').to_owned()).collect();
        self.dane_mx.insert(fqdn(domain), ValidatedMx::Secure(hosts), far_future());
    }

    /// Pins MX records whose signatures do not validate.
    pub fn pin_bogus_mx(&self, domain: &str, exchanges: &[(u16, &str)]) {
        self.pin_unvalidated_mx(domain, exchanges);
        self.dane_mx.insert(fqdn(domain), ValidatedMx::Bogus, far_future());
    }

    /// Pins the validated TLSA records of an MX host, e.g. `["3 1 1 0a1b…"]`; none means it has none.
    pub fn pin_tlsa(&self, host: &str, records: &[&str]) -> Result<(), String> {
        let records: Arc<[Tlsa]> = records
            .iter()
            .map(|record| Tlsa::parse(record).ok_or_else(|| format!("\"{record}\" is not a TLSA record")))
            .collect::<Result<_, _>>()?;
        let found = if records.is_empty() { HostTlsa::None } else { HostTlsa::Records(records) };
        self.tlsa.insert(fqdn(&tlsa_name(host)), found, far_future());
        Ok(())
    }

    /// Pins TLSA records whose signatures do not validate.
    pub fn pin_bogus_tlsa(&self, host: &str) {
        self.tlsa.insert(fqdn(&tlsa_name(host)), HostTlsa::Bogus, far_future());
    }

    pub fn pin_ipv6(&self, host: &str, addresses: &[Ipv6Addr]) {
        let records: Arc<[Ipv6Addr]> = addresses.iter().copied().collect();
        self.ipv6.insert(
            fqdn(host),
            RecordSet { rrset: records, dnssec_status: DnssecStatus::Indeterminate },
            far_future(),
        );
    }

    /// Pins the names an address points back to, e.g. for tests.
    pub fn pin_ptr(&self, ip: IpAddr, hosts: &[&str]) {
        let records: Arc<[Box<str>]> = hosts.iter().map(|host| fqdn(host)).collect();
        self.ptr.insert(ip, RecordSet { rrset: records, dnssec_status: DnssecStatus::Indeterminate }, far_future());
    }

    pub fn pin_ipv4(&self, host: &str, addresses: &[Ipv4Addr]) {
        let records: Arc<[Ipv4Addr]> = addresses.iter().copied().collect();
        self.ipv4.insert(
            fqdn(host),
            RecordSet { rrset: records, dnssec_status: DnssecStatus::Indeterminate },
            far_future(),
        );
    }
}
