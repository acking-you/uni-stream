//! Stream abstractions and implementations for TCP and UDP.

use std::net::SocketAddr;
#[cfg(not(target_os = "windows"))]
use std::os::fd::AsFd;
#[cfg(target_os = "windows")]
use std::os::windows::io::AsSocket;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};

use crate::addr::{each_addr, ToSocketAddrs};
use crate::udp::{UdpListener, UdpStream, UdpStreamReadHalf, UdpStreamWriteHalf};

type Result<T, E = std::io::Error> = std::result::Result<T, E>;

/// A stream that can be split into read and write halves.
pub trait StreamSplit {
    /// Reader half type.
    type ReaderRef<'a>: AsyncReadExt + Send + Unpin
    where
        Self: 'a;
    /// Writer half type.
    type WriterRef<'a>: AsyncWriteExt + Send + Unpin
    where
        Self: 'a;
    /// Owned reader half type.
    type ReaderOwned: AsyncReadExt + Send + Unpin + 'static;
    /// Owned writer half type.
    type WriterOwned: AsyncWriteExt + Send + Unpin + 'static;

    /// Split into reader and writer halves.
    fn split(&mut self) -> (Self::ReaderRef<'_>, Self::WriterRef<'_>);

    /// Split into owned reader and writer halves.
    fn into_split(self) -> (Self::ReaderOwned, Self::WriterOwned);
}

/// Marker trait for streams used in the system.
pub trait NetworkStream:
    StreamSplit + AsyncReadExt + AsyncWriteExt + Send + Unpin + 'static
{
}

macro_rules! gen_stream_impl {
    ($struct_name:ident, $inner_ty:ty) => {
        /// Wrapper type used to implement stream traits.
        pub struct $struct_name($inner_ty);

        impl $struct_name {
            /// Create a new wrapper.
            pub fn new(stream: $inner_ty) -> Self {
                Self(stream)
            }
        }

        impl AsyncRead for $struct_name {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
            }
        }

        impl AsyncWrite for $struct_name {
            fn poll_write(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
                buf: &[u8],
            ) -> std::task::Poll<std::prelude::v1::Result<usize, std::io::Error>> {
                std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
            }

            fn poll_flush(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::prelude::v1::Result<(), std::io::Error>> {
                std::pin::Pin::new(&mut self.0).poll_flush(cx)
            }

            fn poll_shutdown(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::prelude::v1::Result<(), std::io::Error>> {
                std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
            }
        }
    };
}

gen_stream_impl!(TcpStreamImpl, TcpStream);
gen_stream_impl!(UdpStreamImpl, UdpStream);

impl StreamSplit for TcpStreamImpl {
    type ReaderRef<'a> = ReadHalf<'a>
    where
        Self: 'a;
    type WriterRef<'a> = WriteHalf<'a>
    where
        Self: 'a;
    type ReaderOwned = tokio::net::tcp::OwnedReadHalf;
    type WriterOwned = tokio::net::tcp::OwnedWriteHalf;

    fn split(&mut self) -> (Self::ReaderRef<'_>, Self::WriterRef<'_>) {
        self.0.split()
    }

    fn into_split(self) -> (Self::ReaderOwned, Self::WriterOwned) {
        self.0.into_split()
    }
}

impl StreamSplit for UdpStreamImpl {
    type ReaderRef<'a> = UdpStreamReadHalf<'static>;
    type WriterRef<'a> = UdpStreamWriteHalf<'a>
    where
        Self: 'a;
    type ReaderOwned = crate::udp::UdpStreamOwnedReadHalf;
    type WriterOwned = crate::udp::UdpStreamOwnedWriteHalf;

    fn split(&mut self) -> (Self::ReaderRef<'_>, Self::WriterRef<'_>) {
        self.0.split()
    }

    fn into_split(self) -> (Self::ReaderOwned, Self::WriterOwned) {
        self.0.into_split()
    }
}

impl NetworkStream for TcpStreamImpl {}
impl NetworkStream for UdpStreamImpl {}

/// Provides an abstraction for connect.
pub trait StreamProvider {
    /// Stream obtained after connect.
    type Item: NetworkStream;

    /// Create a stream from a socket address or hostname.
    fn from_addr<A: ToSocketAddrs + Send>(
        addr: A,
    ) -> impl std::future::Future<Output = Result<Self::Item>> + Send;
}

/// Provider for TCP connections.
pub struct TcpStreamProvider;

impl StreamProvider for TcpStreamProvider {
    type Item = TcpStreamImpl;

    async fn from_addr<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Item> {
        Ok(TcpStreamImpl(
            each_addr(addr, TcpStream::connect).await?,
        ))
    }
}

/// Provider for UDP connections.
pub struct UdpStreamProvider;

impl StreamProvider for UdpStreamProvider {
    type Item = UdpStreamImpl;

    async fn from_addr<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Item> {
        Ok(UdpStreamImpl(UdpStream::connect(addr).await?))
    }
}

/// Provides an abstraction for bind.
pub trait ListenerProvider {
    /// Listener obtained after bind.
    type Listener: StreamAccept + 'static;

    /// Bind a listener from address/hostname.
    fn bind<A: ToSocketAddrs + Send>(
        addr: A,
    ) -> impl std::future::Future<Output = Result<Self::Listener>> + Send;
}

/// Abstractions for listener-provided operations.
pub trait StreamAccept {
    /// Stream obtained after accept.
    type Item: NetworkStream;

    /// Listener waits to get new Stream.
    fn accept(&self) -> impl std::future::Future<Output = Result<(Self::Item, SocketAddr)>> + Send;
}

/// Provider for TCP listeners.
pub struct TcpListenerProvider;

/// TCP listener wrapper.
pub struct TcpListenerImpl(TcpListener);

impl StreamAccept for TcpListenerImpl {
    type Item = TcpStreamImpl;

    async fn accept(&self) -> Result<(Self::Item, SocketAddr)> {
        let (stream, addr) = self.0.accept().await?;
        Ok((TcpStreamImpl::new(stream), addr))
    }
}

impl ListenerProvider for TcpListenerProvider {
    type Listener = TcpListenerImpl;

    async fn bind<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Listener> {
        Ok(TcpListenerImpl(each_addr(addr, TcpListener::bind).await?))
    }
}

/// Provider for UDP listeners.
pub struct UdpListenerProvider;

/// UDP listener wrapper.
pub struct UdpListenerImpl(UdpListener);

impl StreamAccept for UdpListenerImpl {
    type Item = UdpStreamImpl;

    async fn accept(&self) -> Result<(Self::Item, SocketAddr)> {
        let (stream, addr) = self.0.accept().await?;
        Ok((UdpStreamImpl::new(stream), addr))
    }
}

impl ListenerProvider for UdpListenerProvider {
    type Listener = UdpListenerImpl;

    async fn bind<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Listener> {
        Ok(UdpListenerImpl(UdpListener::bind(addr).await?))
    }
}

/// How long it takes for TCP to start sending keepalive probe packets when no data is exchanged.
const TCP_KEEPALIVE_TIME: Duration = Duration::from_secs(20);
/// Time interval between two consecutive keepalive probe packets.
const TCP_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
/// The number of times a keepalive probe packet is performed to be sent.
#[cfg(not(target_os = "windows"))]
const TCP_KEEPALIVE_PROBES: u32 = 3;

/// Enable TCP keepalive on a socket.
#[cfg(not(target_os = "windows"))]
pub fn set_tcp_keep_alive<S: AsFd>(stream: &S) -> std::result::Result<(), std::io::Error> {
    let sock_ref = socket2::SockRef::from(stream);
    let mut ka = socket2::TcpKeepalive::new();
    ka = ka.with_time(TCP_KEEPALIVE_TIME);
    ka = ka.with_interval(TCP_KEEPALIVE_INTERVAL);
    ka = ka.with_retries(TCP_KEEPALIVE_PROBES);
    sock_ref.set_tcp_keepalive(&ka)
}

/// Enable TCP keepalive on a socket.
#[cfg(target_os = "windows")]
pub fn set_tcp_keep_alive<S: AsSocket>(stream: &S) -> std::result::Result<(), std::io::Error> {
    let sock_ref = socket2::SockRef::from(stream);
    let mut ka = socket2::TcpKeepalive::new();
    ka = ka.with_time(TCP_KEEPALIVE_TIME);
    ka = ka.with_interval(TCP_KEEPALIVE_INTERVAL);
    sock_ref.set_tcp_keepalive(&ka)
}

/// Disable Nagle's algorithm to reduce latency for small packets.
#[cfg(not(target_os = "windows"))]
pub fn set_tcp_nodelay<S: AsFd>(stream: &S) -> std::result::Result<(), std::io::Error> {
    let sock_ref = socket2::SockRef::from(stream);
    sock_ref.set_tcp_nodelay(true)
}

/// Disable Nagle's algorithm to reduce latency for small packets.
#[cfg(target_os = "windows")]
pub fn set_tcp_nodelay<S: AsSocket>(stream: &S) -> std::result::Result<(), std::io::Error> {
    let sock_ref = socket2::SockRef::from(stream);
    sock_ref.set_tcp_nodelay(true)
}

/// Resolve a single socket address from input.
pub async fn got_one_socket_addr<A: ToSocketAddrs>(addr: A) -> Result<SocketAddr> {
    let mut iter = addr.to_socket_addrs().await?;
    iter.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "could not resolve to any addresses",
        )
    })
}
