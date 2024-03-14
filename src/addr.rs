//! Provide domain name resolution service

use std::future::{self, Future};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::pin::Pin;
use std::sync::RwLock;
use std::task::{ready, Context, Poll};

use once_cell::sync::Lazy;
use tokio::task::JoinHandle;
use trust_dns_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
use trust_dns_resolver::Resolver;

type Result<T, E = std::io::Error> = std::result::Result<T, E>;
type ReadyFuture<T> = future::Ready<Result<T>>;

macro_rules! invalid_input {
    ($msg:expr) => {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, $msg)
    };
}

macro_rules! try_opt {
    ($call:expr, $msg:expr) => {
        match $call {
            Some(v) => v,
            None => Err(invalid_input!($msg))?,
        }
    };
}

macro_rules! try_ret {
    ($call:expr, $msg:expr) => {
        match $call {
            Ok(v) => v,
            Err(e) => Err(invalid_input!(format!("{} ,detail:{e}", $msg)))?,
        }
    };
}

/// Converts or resolves without blocking to one or more `SocketAddr` values.
///
/// # DNS
///
/// Implemented custom DNS resolution for string type `ToSocketAddrs`,
/// user can change default dns resolution server via [`set_custom_dns_server`].
pub trait ToSocketAddrs {
    /// An iterator over SocketAddr
    type Iter: Iterator<Item = SocketAddr> + Send + 'static;
    /// Future representing an iterator
    type Future: Future<Output = Result<Self::Iter>> + Send + 'static;

    /// Returns an asynchronous iterator for getting `SocketAddr`
    fn to_socket_addrs(&self) -> Self::Future;
}

impl ToSocketAddrs for SocketAddr {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        let iter = Some(*self).into_iter();
        future::ready(Ok(iter))
    }
}

impl ToSocketAddrs for SocketAddrV4 {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        SocketAddr::V4(*self).to_socket_addrs()
    }
}

impl ToSocketAddrs for SocketAddrV6 {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        SocketAddr::V6(*self).to_socket_addrs()
    }
}

impl ToSocketAddrs for (IpAddr, u16) {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        let iter = Some(SocketAddr::from(*self)).into_iter();
        future::ready(Ok(iter))
    }
}

impl ToSocketAddrs for (Ipv4Addr, u16) {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        let (ip, port) = *self;
        SocketAddrV4::new(ip, port).to_socket_addrs()
    }
}

impl ToSocketAddrs for (Ipv6Addr, u16) {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        let (ip, port) = *self;
        SocketAddrV6::new(ip, port, 0, 0).to_socket_addrs()
    }
}

impl ToSocketAddrs for &[SocketAddr] {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::vec::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        #[inline]
        fn slice_to_vec(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
            addrs.to_vec()
        }

        // This uses a helper method because clippy doesn't like the `to_vec()`
        // call here (it will allocate, whereas `self.iter().copied()` would
        // not), but it's actually necessary in order to ensure that the
        // returned iterator is valid for the `'static` lifetime, which the
        // borrowed `slice::Iter` iterator would not be.
        let iter = slice_to_vec(self).into_iter();
        future::ready(Ok(iter))
    }
}

/// Represents one or more SockeAddr, since a String type may be a domain name or a direct address
#[derive(Debug)]
pub enum OneOrMore {
    /// Direct address
    One(std::option::IntoIter<SocketAddr>),
    /// Addresses resolved by dns
    More(std::vec::IntoIter<SocketAddr>),
}

#[derive(Debug)]
enum State {
    Ready(Option<SocketAddr>),
    Blocking(JoinHandle<Result<std::vec::IntoIter<SocketAddr>>>),
}

/// Implement Future to return asynchronous results
#[derive(Debug)]
pub struct MaybeReady(State);

impl Future for MaybeReady {
    type Output = Result<OneOrMore>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.0 {
            State::Ready(ref mut i) => {
                let iter = OneOrMore::One(i.take().into_iter());
                Poll::Ready(Ok(iter))
            }
            State::Blocking(ref mut rx) => {
                let res = ready!(Pin::new(rx).poll(cx))?.map(OneOrMore::More);

                Poll::Ready(res)
            }
        }
    }
}

impl Iterator for OneOrMore {
    type Item = SocketAddr;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            OneOrMore::One(i) => i.next(),
            OneOrMore::More(i) => i.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            OneOrMore::One(i) => i.size_hint(),
            OneOrMore::More(i) => i.size_hint(),
        }
    }
}

// ===== impl &str =====

impl ToSocketAddrs for str {
    type Future = MaybeReady;
    type Iter = OneOrMore;

    fn to_socket_addrs(&self) -> Self::Future {
        // First check if the input parses as a socket address
        let res: Result<SocketAddr, _> = self.parse();
        if let Ok(addr) = res {
            return MaybeReady(State::Ready(Some(addr)));
        }

        // Run DNS lookup on the blocking pool
        let s = self.to_owned();

        MaybeReady(State::Blocking(tokio::task::spawn_blocking(move || {
            // Customized dns resolvers are preferred, if a custom resolver does not exist then the
            // standard library's
            get_socket_addrs_inner(&s).map(|v| v.into_iter())
        })))
    }
}

/// Implement this trait for &T of type !Sized(such as str), since &T of type Sized all implement it
/// by default.
impl<T> ToSocketAddrs for &T
where
    T: ToSocketAddrs + ?Sized,
{
    type Future = T::Future;
    type Iter = T::Iter;

    fn to_socket_addrs(&self) -> Self::Future {
        (**self).to_socket_addrs()
    }
}

// ===== impl (&str,u16) =====

impl ToSocketAddrs for (&str, u16) {
    type Future = MaybeReady;
    type Iter = OneOrMore;

    fn to_socket_addrs(&self) -> Self::Future {
        let (host, port) = *self;

        // try to parse the host as a regular IP address first
        if let Ok(addr) = host.parse::<Ipv4Addr>() {
            let addr = SocketAddrV4::new(addr, port);
            let addr = SocketAddr::V4(addr);

            return MaybeReady(State::Ready(Some(addr)));
        }

        if let Ok(addr) = host.parse::<Ipv6Addr>() {
            let addr = SocketAddrV6::new(addr, port, 0, 0);
            let addr = SocketAddr::V6(addr);

            return MaybeReady(State::Ready(Some(addr)));
        }

        let host = host.to_owned();

        MaybeReady(State::Blocking(tokio::task::spawn_blocking(move || {
            get_socket_addrs_from_host_port_inner(&host, port).map(|v| v.into_iter())
        })))
    }
}

// ===== impl (String,u16) =====

impl ToSocketAddrs for (String, u16) {
    type Future = MaybeReady;
    type Iter = OneOrMore;

    fn to_socket_addrs(&self) -> Self::Future {
        (self.0.as_str(), self.1).to_socket_addrs()
    }
}

// ===== impl String =====

impl ToSocketAddrs for String {
    type Future = <str as ToSocketAddrs>::Future;
    type Iter = <str as ToSocketAddrs>::Iter;

    fn to_socket_addrs(&self) -> Self::Future {
        self[..].to_socket_addrs()
    }
}

/// Default dns resolution server
const DEFAULT_DNS_SERVER_GROUP: &[IpAddr] = &[
    IpAddr::V4(Ipv4Addr::new(223, 5, 5, 5)), // alibaba
    IpAddr::V4(Ipv4Addr::new(223, 6, 6, 6)),
    IpAddr::V4(Ipv4Addr::new(119, 29, 29, 29)), // tencent
    IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),      // google
    IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888)), // google
];

/// Customized dns resolution server
static DNS_SERVER_GROUP: Lazy<RwLock<Vec<IpAddr>>> =
    Lazy::new(|| RwLock::new(DEFAULT_DNS_SERVER_GROUP.to_vec()));

const DNS_QUERY_PORT: u16 = 53;

#[inline]
fn get_custom_resolver() -> Result<Resolver> {
    let dns_group = try_ret!(DNS_SERVER_GROUP.read(), "read dns server");
    Resolver::new(
        ResolverConfig::from_parts(
            None,
            vec![],
            NameServerConfigGroup::from_ips_clear(&dns_group, DNS_QUERY_PORT, true),
        ),
        ResolverOpts::default(),
    )
    .map_err(|e| invalid_input!(format!("create custom resolver error:{e}")))
}

/// Set up DNS servers, use `DEFAULT_DNS_SERVER_GROUP` by default
/// Note: must be called before the first network connection to be effective
#[inline]
pub fn set_custom_dns_server(dns_addrs: &[IpAddr]) -> Result<()> {
    let mut writer = DNS_SERVER_GROUP
        .write()
        .map_err(|e| invalid_input!(format!("get dns server writer, detail:{e}")))?;
    let servers: &mut Vec<IpAddr> = writer.as_mut();
    servers.clear();
    dns_addrs.iter().for_each(|&a| servers.push(a));
    Ok(())
}

/// Resolving domain to get `IpAddr`
/// Note: must run as async runtime,such as [`tokio::task::spawn_blocking`]
pub async fn get_ip_addrs(s: &str) -> Result<Vec<IpAddr>> {
    let s = s.to_owned();
    tokio::task::spawn_blocking(move || get_ip_addrs_inner(&s))
        .await
        .map_err(|_| invalid_input!("get ip addrs"))?
}

/// Resolving domain to get `IpAddr`
/// Note: must run as async runtime,such as [`tokio::task::spawn_blocking`]
fn get_ip_addrs_inner(s: &str) -> Result<Vec<IpAddr>> {
    thread_local! {
        static RESOLVER:Option<Resolver> = {
            match get_custom_resolver(){
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::error!("create resolver error:{e}");
                    None
                },
            }
        };
    }
    let result = RESOLVER.with(|r| r.as_ref().map(|r| r.lookup_ip(s)));
    try_opt!(result, "custom resolver not exist")
        .map(|v| v.into_iter().collect())
        .map_err(|e| invalid_input!(e))
}

/// Resolving domain and port to get `SocketAddr`
#[inline]
pub async fn get_socket_addrs_from_host_port(s: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let s = s.to_owned();
    tokio::task::spawn_blocking(move || get_socket_addrs_from_host_port_inner(&s, port))
        .await
        .map_err(|_| invalid_input!("get socket addrs from host port"))?
}

/// Resolving domain and port to get `SocketAddr`
/// Note: must run as async runtime,such as [`tokio::task::spawn_blocking`]
#[inline]
fn get_socket_addrs_from_host_port_inner(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    match get_ip_addrs_inner(host) {
        Ok(r) => Ok(r.into_iter().map(|ip| SocketAddr::new(ip, port)).collect()),
        // Resolve dns properly with the standard library
        Err(_) => std::net::ToSocketAddrs::to_socket_addrs(&(host, port)).map(|v| v.collect()),
    }
}

/// Resolving `domain:port` forms,such as bilibili.com:1080
#[inline]
pub async fn get_socket_addrs(s: &str) -> Result<Vec<SocketAddr>> {
    let s = s.to_owned();
    tokio::task::spawn_blocking(move || get_socket_addrs_inner(&s))
        .await
        .map_err(|_| invalid_input!("get socket addrs"))?
}

/// Resolving `domain:port` forms,such as bilibili.com:1080
/// Note: must run as async runtime,such as [`tokio::task::spawn_blocking`]
#[inline]
fn get_socket_addrs_inner(s: &str) -> Result<Vec<SocketAddr>> {
    let (host, port_str) = try_opt!(s.rsplit_once(':'), "invalid socket address");
    let port: u16 = try_opt!(port_str.parse().ok(), "invalid port value");
    get_socket_addrs_from_host_port_inner(host, port)
}

/// Look up all the socket addr's and pass in the method to get the result
pub async fn each_addr<A: ToSocketAddrs, F, T, R>(addr: A, f: F) -> Result<T>
where
    F: Fn(SocketAddr) -> R,
    R: std::future::Future<Output = Result<T>>,
{
    let addrs = match addr.to_socket_addrs().await {
        Ok(addrs) => addrs,
        Err(e) => return Err(e),
    };
    let mut last_err = None;
    for addr in addrs {
        match f(addr).await {
            Ok(l) => return Ok(l),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "could not resolve to any addresses",
        )
    }))
}
