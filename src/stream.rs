//! Provides an abstraction of Stream, as well as specific implementations for TCP and UDP

use std::net::SocketAddr;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};

use crate::udp::UdpListener;

use super::addr::{each_addr, ToSocketAddrs};
use super::udp::{UdpStream, UdpStreamReadHalf, UdpStreamWriteHalf};

type Result<T, E = std::io::Error> = std::result::Result<T, E>;

/// Used to abstract Stream operations, see [`tokio::net::TcpStream`] for details
pub trait NetworkStream: AsyncReadExt + AsyncWriteExt + Send + Unpin + 'static {
    /// The reader association type used to represent the read operation
    type ReaderRef<'a>: AsyncReadExt + Send + Unpin + Send
    where
        Self: 'a;
    /// The writer association type used to represent the write operation
    type WriterRef<'a>: AsyncWriteExt + Send + Unpin + Send
    where
        Self: 'a;

    /// Used to get internal specific implementations such as [`UdpStream`]
    type InnerStream: AsyncReadExt + AsyncWriteExt + Unpin + Send;

    /// Splitting the Stream into a read side and a write side is useful in scenarios where you need to use read and write separately.
    fn split(&mut self) -> (Self::ReaderRef<'_>, Self::WriterRef<'_>);

    /// Get the internal concrete implementation, note that this operation transfers ownership
    fn into_inner_stream(self) -> Self::InnerStream;

    /// get  local address
    fn local_addr(&self) -> Result<SocketAddr>;

    /// get  peer address
    fn peer_addr(&self) -> Result<SocketAddr>;
}

macro_rules! gen_stream_impl {
    ($struct_name:ident, $inner_ty:ty,$doc_string:literal) => {
        #[doc = $doc_string]
        pub struct $struct_name($inner_ty);

        impl $struct_name {
            /// create new struct
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

gen_stream_impl!(
    TcpStreamImpl,
    TcpStream,
    "Implementing NetworkStream for TcpStream"
);

gen_stream_impl!(
    UdpStreamImpl,
    UdpStream,
    "Implementing NetworkStream for UdpStream"
);

impl NetworkStream for TcpStreamImpl {
    type ReaderRef<'a> = ReadHalf<'a>
    where
        Self: 'a;

    type WriterRef<'a> = WriteHalf<'a>
    where
        Self: 'a;

    type InnerStream = TcpStream;

    fn split(&mut self) -> (Self::ReaderRef<'_>, Self::WriterRef<'_>) {
        self.0.split()
    }

    fn into_inner_stream(self) -> Self::InnerStream {
        self.0
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.0.local_addr()
    }

    fn peer_addr(&self) -> Result<SocketAddr> {
        self.0.peer_addr()
    }
}

impl NetworkStream for UdpStreamImpl {
    type ReaderRef<'a> = UdpStreamReadHalf<'static>;

    type WriterRef<'a> = UdpStreamWriteHalf<'a>
    where
        Self: 'a;

    type InnerStream = UdpStream;

    fn split(&mut self) -> (Self::ReaderRef<'_>, Self::WriterRef<'_>) {
        self.0.split()
    }

    fn into_inner_stream(self) -> Self::InnerStream {
        self.0
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.0.local_addr()
    }

    fn peer_addr(&self) -> Result<SocketAddr> {
        self.0.peer_addr()
    }
}

/// Provides an abstraction for connect
pub trait StreamProvider {
    /// Stream obtained after connect
    type Item: NetworkStream;

    /// Getting the Stream through a connection,
    /// the only difference between this process and tokio::net::TcpStream::connect is that
    /// it will be resolved through a customized dns service
    fn connect<A: ToSocketAddrs + Send>(
        addr: A,
    ) -> impl std::future::Future<Output = Result<Self::Item>> + Send;
}

/// The medium used to get the [`TcpStreamImpl`]
pub struct TcpStreamProvider;

impl StreamProvider for TcpStreamProvider {
    type Item = TcpStreamImpl;

    async fn connect<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Item> {
        Ok(TcpStreamImpl(each_addr(addr, TcpStream::connect).await?))
    }
}

/// The medium used to get the [`UdpStreamImpl`]
pub struct UdpStreamProvider;

impl StreamProvider for UdpStreamProvider {
    type Item = UdpStreamImpl;

    async fn connect<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Item> {
        Ok(UdpStreamImpl(UdpStream::connect(addr).await?))
    }
}

/// Provides an abstraction for bind
pub trait ListenerProvider {
    /// Listener obtained after bind
    type Listener: StreamAccept + 'static;

    /// Getting the Listener through a binding,
    /// the only difference between this process and `tokio::net::TcpListener::bind` is that
    /// it will be resolved through a customized dns service
    fn bind<A: ToSocketAddrs + Send>(
        addr: A,
    ) -> impl std::future::Future<Output = Result<Self::Listener>> + Send;
}

/// Abstractions for Listener-provided operations
pub trait StreamAccept {
    /// Stream obtained after accept
    type Item: NetworkStream;

    /// Listener waits to get new Stream
    fn accept(&self) -> impl std::future::Future<Output = Result<(Self::Item, SocketAddr)>> + Send;
}

/// The medium used to get the [`TcpListenerImpl`]
pub struct TcpListenerProvider;

impl ListenerProvider for TcpListenerProvider {
    type Listener = TcpListenerImpl;

    async fn bind<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Listener> {
        Ok(TcpListenerImpl(each_addr(addr, TcpListener::bind).await?))
    }
}

/// Implementing [`StreamAccept`] for TcpListener
pub struct TcpListenerImpl(TcpListener);

impl StreamAccept for TcpListenerImpl {
    type Item = TcpStreamImpl;

    async fn accept(&self) -> Result<(Self::Item, SocketAddr)> {
        let (stream, addr) = self.0.accept().await?;
        Ok((TcpStreamImpl::new(stream), addr))
    }
}

/// The medium used to get the [`TcpListenerImpl`]
pub struct UdpListenerProvider;

impl ListenerProvider for UdpListenerProvider {
    type Listener = UdpListenerImpl;

    async fn bind<A: ToSocketAddrs + Send>(addr: A) -> Result<Self::Listener> {
        Ok(UdpListenerImpl(UdpListener::bind(addr).await?))
    }
}

/// Implementing [`StreamAccept`] for [`UdpListener`]
pub struct UdpListenerImpl(UdpListener);

impl StreamAccept for UdpListenerImpl {
    type Item = UdpStreamImpl;

    async fn accept(&self) -> Result<(Self::Item, SocketAddr)> {
        let (stream, addr) = self.0.accept().await?;
        Ok((UdpStreamImpl::new(stream), addr))
    }
}
