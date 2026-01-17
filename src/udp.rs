//! Provides a `tokio::TcpStream`-like UDP stream implementation based on `tokio::UdpSocket`.

use std::fmt::Debug;
use std::future::Future;
use std::io::{self};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use futures::future::poll_fn;
use futures::Stream;
use hashbrown::HashMap;
use kanal::{AsyncReceiver, AsyncSender, ReceiveStreamOwned};
use socket2::SockRef;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UdpSocket;
#[cfg(feature = "udp-timeout")]
use tokio::time::Instant;
#[cfg(feature = "udp-timeout")]
use tokio::time::Sleep;

use self::impl_inner::{UdpStreamReadContext, UdpStreamWriteContext};
use super::addr::{each_addr, ToSocketAddrs};
#[cfg(feature = "udp-timeout")]
use crate::udp::impl_inner::get_sleep;

const UDP_CHANNEL_LEN: usize = 100;
const UDP_BUFFER_SIZE: usize = 65_507;
const UDP_SOCKET_BUFFER_BYTES: usize = 4 * 1024 * 1024;

type Result<T, E = std::io::Error> = std::result::Result<T, E>;

fn receiver_stream<T: Send + 'static>(
    receiver: AsyncReceiver<T>,
) -> Pin<Box<ReceiveStreamOwned<T>>> {
    Box::pin(receiver.into_stream())
}

#[cfg(not(target_os = "windows"))]
/// Tune UDP socket buffer sizes for better throughput.
pub fn tune_udp_socket(socket: &UdpSocket) {
    let sock_ref = SockRef::from(socket);
    if let Err(err) = sock_ref.set_recv_buffer_size(UDP_SOCKET_BUFFER_BYTES) {
        tracing::warn!("failed to set udp recv buffer size: {err}");
    }
    if let Err(err) = sock_ref.set_send_buffer_size(UDP_SOCKET_BUFFER_BYTES) {
        tracing::warn!("failed to set udp send buffer size: {err}");
    }
}

#[cfg(target_os = "windows")]
/// Tune UDP socket buffer sizes for better throughput.
pub fn tune_udp_socket(socket: &UdpSocket) {
    let sock_ref = SockRef::from(socket);
    if let Err(err) = sock_ref.set_recv_buffer_size(UDP_SOCKET_BUFFER_BYTES) {
        tracing::warn!("failed to set udp recv buffer size: {err}");
    }
    if let Err(err) = sock_ref.set_send_buffer_size(UDP_SOCKET_BUFFER_BYTES) {
        tracing::warn!("failed to set udp send buffer size: {err}");
    }
}

macro_rules! error_get_or_continue {
    ($func_call:expr, $msg:expr) => {
        match $func_call {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("{}, detail:{e}", $msg);
                continue;
            }
        }
    };
}

mod impl_inner {
    #[cfg(feature = "udp-timeout")]
    use std::time::Duration;

    #[cfg(feature = "udp-timeout")]
    use futures::FutureExt;
    #[cfg(feature = "udp-timeout")]
    use once_cell::sync::Lazy;
    #[cfg(feature = "udp-timeout")]
    use tokio::time::{sleep, Instant};

    use super::*;

    pub(super) trait UdpStreamReadContext {
        fn get_mut_remaining_bytes(&mut self) -> &mut Option<Bytes>;
        fn get_receiver_stream(&mut self) -> &mut Pin<Box<ReceiveStreamOwned<Bytes>>>;
        #[cfg(feature = "udp-timeout")]
        fn get_timeout(&mut self) -> &mut Pin<Box<Sleep>>;
    }

    pub(super) trait UdpStreamWriteContext {
        fn is_connect(&self) -> bool;
        fn get_socket(&self) -> &tokio::net::UdpSocket;
        fn get_peer_addr(&self) -> &SocketAddr;
    }

    pub(super) fn poll_read<T: UdpStreamReadContext>(
        mut read_ctx: T,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<Result<()>> {
        // timeout
        #[cfg(feature = "udp-timeout")]
        if read_ctx.get_timeout().poll_unpin(cx).is_ready() {
            buf.clear();
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "UdpStream timeout with duration:{:?}",
                    get_timeout_duration()
                ),
            )));
        }

        #[cfg(feature = "udp-timeout")]
        #[inline]
        fn update_timeout(timeout: &mut Pin<Box<Sleep>>) {
            timeout
                .as_mut()
                .reset(Instant::now() + get_timeout_duration())
        }

        let is_consume_remaining = if let Some(remaining) = read_ctx.get_mut_remaining_bytes() {
            if buf.remaining() < remaining.len() {
                buf.put_slice(&remaining.split_to(buf.remaining())[..]);
            } else {
                buf.put_slice(&remaining[..]);
                *read_ctx.get_mut_remaining_bytes() = None;
            }
            true
        } else {
            false
        };

        if is_consume_remaining {
            #[cfg(feature = "udp-timeout")]
            update_timeout(read_ctx.get_timeout());
            return Poll::Ready(Ok(()));
        }

        let remaining = match read_ctx.get_receiver_stream().as_mut().poll_next(cx) {
            Poll::Ready(Some(mut inner_buf)) => {
                let remaining = if buf.remaining() < inner_buf.len() {
                    Some(inner_buf.split_off(buf.remaining()))
                } else {
                    None
                };
                buf.put_slice(&inner_buf[..]);
                remaining
            }
            Poll::Ready(None) => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Broken pipe",
                )));
            }
            Poll::Pending => return Poll::Pending,
        };
        #[cfg(feature = "udp-timeout")]
        update_timeout(read_ctx.get_timeout());
        *read_ctx.get_mut_remaining_bytes() = remaining;
        Poll::Ready(Ok(()))
    }

    pub(super) fn poll_write<T: UdpStreamWriteContext>(
        write_ctx: T,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        if write_ctx.is_connect() {
            write_ctx.get_socket().poll_send(cx, buf)
        } else {
            write_ctx
                .get_socket()
                .poll_send_to(cx, buf, *write_ctx.get_peer_addr())
        }
    }

    #[cfg(feature = "udp-timeout")]
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

    #[cfg(feature = "udp-timeout")]
    static mut CUSTOM_TIMEOUT: Option<Duration> = None;

    /// Set custom timeout.
    /// Note that this function can only be called before the [`TIMEOUT`] lazy variable is created.
    #[cfg(feature = "udp-timeout")]
    pub fn set_custom_timeout(timeout: Duration) {
        unsafe { CUSTOM_TIMEOUT = Some(timeout) }
    }

    #[cfg(feature = "udp-timeout")]
    static TIMEOUT: Lazy<Duration> = Lazy::new(|| match unsafe { CUSTOM_TIMEOUT } {
        Some(dur) => dur,
        None => DEFAULT_TIMEOUT,
    });

    #[cfg(feature = "udp-timeout")]
    #[inline]
    pub(super) fn get_timeout_duration() -> Duration {
        *TIMEOUT
    }

    #[cfg(feature = "udp-timeout")]
    #[inline]
    pub(super) fn get_sleep() -> Sleep {
        sleep(get_timeout_duration())
    }
}

#[cfg(feature = "udp-timeout")]
pub use impl_inner::set_custom_timeout;

/// An I/O object representing a UDP socket listening for incoming connections.
///
/// This object can be converted into a stream of incoming connections for
/// various forms of processing.
pub struct UdpListener {
    handler: tokio::task::JoinHandle<()>,
    receiver: AsyncReceiver<(UdpStream, SocketAddr)>,
    local_addr: SocketAddr,
}

impl Drop for UdpListener {
    fn drop(&mut self) {
        self.handler.abort();
    }
}

impl UdpListener {
    /// Usage is exactly the same as [`tokio::net::TcpListener::bind`]
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        each_addr(addr, UdpListener::bind_inner).await
    }

    async fn bind_inner(local_addr: SocketAddr) -> Result<Self> {
        let (listener_tx, listener_rx) = kanal::bounded_async(UDP_CHANNEL_LEN);
        let udp_socket = UdpSocket::bind(local_addr).await?;
        tune_udp_socket(&udp_socket);
        let local_addr = udp_socket.local_addr()?;

        let handler = tokio::spawn(async move {
            let mut streams: HashMap<SocketAddr, AsyncSender<Bytes>> = HashMap::new();
            let socket = Arc::new(udp_socket);
            let (drop_tx, drop_rx) = kanal::bounded_async(10);
            let mut drop_buf = Vec::with_capacity(10);

            let mut buf = BytesMut::with_capacity(UDP_BUFFER_SIZE * 3);
            loop {
                if buf.capacity() < UDP_BUFFER_SIZE {
                    buf.reserve(UDP_BUFFER_SIZE * 3);
                }
                buf.clear();
                tokio::select! {
                    result = drop_rx.drain_into_blocking(&mut drop_buf) => {
                        match result {
                            Ok(_) => {
                                for peer_addr in drop_buf.drain(..) {
                                    streams.remove(&peer_addr);
                                }
                            }
                            Err(err) => {
                                tracing::error!("UdpListener cleanup recv error: {err}");
                                drop_buf.clear();
                            }
                        }
                    }
                    ret = socket.recv_buf_from(&mut buf) => {
                        let (len,peer_addr) = error_get_or_continue!(ret,"UdpListener `recv_buf_from`");
                        tracing::debug!("udp listener recv {len} bytes from {peer_addr}");
                        match streams.get(&peer_addr) {
                            Some(tx) => {
                                if let Err(err) =  tx.send(buf.copy_to_bytes(len)).await{
                                    tracing::error!("UDPListener send msg to conn, detail:{err}");
                                    streams.remove(&peer_addr);
                                    continue;
                                }
                            }
                            None => {
                                let (child_tx, child_rx) = kanal::bounded_async(UDP_CHANNEL_LEN);
                                // pre send msg
                                error_get_or_continue!(
                                    child_tx.send(buf.copy_to_bytes(len)).await,
                                    "new conn pre send msg"
                                );

                                let udp_stream = UdpStream {
                                    is_connect: false,
                                    local_addr,
                                    peer_addr,
                                    #[cfg(feature = "udp-timeout")]
                                    timeout: Box::pin(get_sleep()),
                                    recv_stream: receiver_stream(child_rx.clone()),
                                    receiver: child_rx,
                                    socket: socket.clone(),
                                    _handler_guard: None,
                                    _listener_guard: Some(ListenerCleanGuard {
                                        sender: drop_tx.clone(),
                                        peer_addr,
                                    }),
                                    remaining: None,
                                };
                                error_get_or_continue!(
                                    listener_tx.send((udp_stream, peer_addr)).await,
                                    "register UDPStream"
                                );
                                streams.insert(peer_addr, child_tx);
                            }
                        }
                    }
                }
            }
        });
        Ok(Self {
            handler,
            receiver: listener_rx,
            local_addr,
        })
    }

    /// Returns the local address that this socket is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    /// Accepts a new incoming UDP connection.
    pub async fn accept(&self) -> io::Result<(UdpStream, SocketAddr)> {
        self.receiver
            .recv()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }
}

#[derive(Debug)]
struct TaskJoinHandleGuard(tokio::task::JoinHandle<()>);

#[derive(Debug, Clone)]
struct ListenerCleanGuard {
    sender: AsyncSender<SocketAddr>,
    peer_addr: SocketAddr,
}

impl Drop for ListenerCleanGuard {
    fn drop(&mut self) {
        let _ = self.sender.try_send(self.peer_addr);
    }
}

impl Drop for TaskJoinHandleGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// An I/O object representing a UDP stream connected to a remote endpoint.
///
/// A UDP stream can either be created by connecting to an endpoint, via the
/// [`UdpStream::connect`] method, or by [UdpListener::accept] a connection from a listener.
pub struct UdpStream {
    is_connect: bool,
    local_addr: SocketAddr,
    peer_addr: SocketAddr,
    socket: Arc<tokio::net::UdpSocket>,
    receiver: AsyncReceiver<Bytes>,
    #[cfg(feature = "udp-timeout")]
    timeout: Pin<Box<Sleep>>,
    recv_stream: Pin<Box<ReceiveStreamOwned<Bytes>>>,
    remaining: Option<Bytes>,
    _handler_guard: Option<TaskJoinHandleGuard>,
    _listener_guard: Option<ListenerCleanGuard>,
}

impl UdpStream {
    /// Create a new UDP stream connected to the specified address.
    ///
    /// This function will create a new UDP socket and attempt to connect it to
    /// the `addr` provided. The returned future will be resolved once the
    /// stream has successfully connected, or it will return an error if one
    /// occurs.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        each_addr(addr, UdpStream::connect_inner).await
    }

    async fn connect_inner(addr: SocketAddr) -> Result<Self> {
        let local_addr: SocketAddr = if addr.is_ipv4() {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        };
        let socket = UdpSocket::bind(local_addr).await?;
        tune_udp_socket(&socket);
        socket.connect(&addr).await?;
        Self::from_tokio(socket, true).await
    }

    /// Creates a new UdpStream from a tokio::net::UdpSocket.
    /// This function is intended to be used to wrap a UDP socket from the tokio library.
    /// Note: The UdpSocket must have the UdpSocket::connect method called before invoking this
    /// function.
    async fn from_tokio(socket: UdpSocket, is_connect: bool) -> Result<Self> {
        tune_udp_socket(&socket);
        let socket = Arc::new(socket);

        let local_addr = socket.local_addr()?;
        let peer_addr = socket.peer_addr()?;

        let (tx, rx) = kanal::bounded_async(UDP_CHANNEL_LEN);

        let socket_inner = socket.clone();

        let handler = tokio::spawn(async move {
            let mut buf = BytesMut::with_capacity(UDP_BUFFER_SIZE);
            loop {
                if buf.capacity() < UDP_BUFFER_SIZE {
                    buf.reserve(UDP_BUFFER_SIZE * 3);
                }
                buf.clear();
                let (len, received_addr) = match socket_inner.recv_buf_from(&mut buf).await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                if received_addr != peer_addr {
                    continue;
                }
                if tx.send(buf.copy_to_bytes(len)).await.is_err() {
                    drop(tx);
                    break;
                }
            }
        });

        Ok(UdpStream {
            local_addr,
            peer_addr,
            #[cfg(feature = "udp-timeout")]
            timeout: Box::pin(get_sleep()),
            recv_stream: receiver_stream(rx.clone()),
            receiver: rx,
            socket,
            _handler_guard: Some(TaskJoinHandleGuard(handler)),
            _listener_guard: None,
            remaining: None,
            is_connect,
        })
    }

    /// Return peer address
    pub fn peer_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.peer_addr)
    }

    /// Return local address
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    /// Split into read side and write side to avoid borrow check, note that ownership is not
    /// transferred
    pub fn split(&self) -> (UdpStreamReadHalf, UdpStreamWriteHalf<'_>) {
        (
            UdpStreamReadHalf {
                recv_stream: receiver_stream(self.receiver.clone()),
                remaining: self.remaining.clone(),
                #[cfg(feature = "udp-timeout")]
                timeout: Box::pin(get_sleep()),
            },
            UdpStreamWriteHalf {
                is_connect: self.is_connect,
                socket: &self.socket,
                peer_addr: self.peer_addr,
            },
        )
    }

    /// Split into owned read and write halves.
    pub fn into_split(self) -> (UdpStreamOwnedReadHalf, UdpStreamOwnedWriteHalf) {
        let guard = Arc::new(UdpStreamGuard {
            _handler_guard: self._handler_guard,
            _listener_guard: self._listener_guard,
        });
        (
            UdpStreamOwnedReadHalf {
                recv_stream: self.recv_stream,
                remaining: self.remaining,
                #[cfg(feature = "udp-timeout")]
                timeout: self.timeout,
                _guard: guard.clone(),
            },
            UdpStreamOwnedWriteHalf {
                is_connect: self.is_connect,
                socket: self.socket,
                peer_addr: self.peer_addr,
                _guard: guard,
            },
        )
    }

    /// Send a single UDP datagram to the connected peer.
    pub async fn send_datagram(&self, data: &[u8]) -> io::Result<()> {
        let sent = if self.is_connect {
            self.socket.send(data).await?
        } else {
            self.socket.send_to(data, self.peer_addr).await?
        };
        if sent != data.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "udp datagram truncated",
            ));
        }
        Ok(())
    }
}

impl UdpStreamReadContext for std::pin::Pin<&mut UdpStream> {
    fn get_mut_remaining_bytes(&mut self) -> &mut Option<Bytes> {
        &mut self.remaining
    }

    fn get_receiver_stream(&mut self) -> &mut Pin<Box<ReceiveStreamOwned<Bytes>>> {
        &mut self.recv_stream
    }

    #[cfg(feature = "udp-timeout")]
    fn get_timeout(&mut self) -> &mut Pin<Box<Sleep>> {
        &mut self.timeout
    }
}

impl UdpStreamWriteContext for std::pin::Pin<&mut UdpStream> {
    fn get_socket(&self) -> &tokio::net::UdpSocket {
        &self.socket
    }

    fn get_peer_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }

    fn is_connect(&self) -> bool {
        self.is_connect
    }
}

impl AsyncRead for UdpStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context, buf: &mut ReadBuf) -> Poll<Result<()>> {
        impl_inner::poll_read(self, cx, buf)
    }
}

impl AsyncWrite for UdpStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {
        impl_inner::poll_write(self, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// [`UdpStream`] read-side implementation
pub struct UdpStreamReadHalf {
    recv_stream: Pin<Box<ReceiveStreamOwned<Bytes>>>,
    remaining: Option<Bytes>,
    #[cfg(feature = "udp-timeout")]
    timeout: Pin<Box<Sleep>>,
}

impl UdpStreamReadContext for std::pin::Pin<&mut UdpStreamReadHalf> {
    fn get_mut_remaining_bytes(&mut self) -> &mut Option<Bytes> {
        &mut self.remaining
    }

    fn get_receiver_stream(&mut self) -> &mut Pin<Box<ReceiveStreamOwned<Bytes>>> {
        &mut self.recv_stream
    }

    #[cfg(feature = "udp-timeout")]
    fn get_timeout(&mut self) -> &mut Pin<Box<Sleep>> {
        &mut self.timeout
    }
}

impl UdpStreamReadContext for std::pin::Pin<&mut UdpStreamOwnedReadHalf> {
    fn get_mut_remaining_bytes(&mut self) -> &mut Option<Bytes> {
        &mut self.remaining
    }

    fn get_receiver_stream(&mut self) -> &mut Pin<Box<ReceiveStreamOwned<Bytes>>> {
        &mut self.recv_stream
    }

    #[cfg(feature = "udp-timeout")]
    fn get_timeout(&mut self) -> &mut Pin<Box<Sleep>> {
        &mut self.timeout
    }
}

impl AsyncRead for UdpStreamReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        impl_inner::poll_read(self, cx, buf)
    }
}

impl AsyncRead for UdpStreamOwnedReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        impl_inner::poll_read(self, cx, buf)
    }
}

impl UdpStreamReadHalf {
    /// Receive a single UDP datagram as an owned buffer.
    pub async fn recv_datagram(&mut self) -> io::Result<Bytes> {
        if self.remaining.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "udp stream has buffered bytes; cannot recv datagram",
            ));
        }

        #[cfg(feature = "udp-timeout")]
        let result = poll_fn(|cx| {
            if self.timeout.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "UdpStream timeout with duration:{:?}",
                        impl_inner::get_timeout_duration()
                    ),
                )));
            }
            match self.recv_stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(msg)) => Poll::Ready(Ok(msg)),
                Poll::Ready(None) => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Broken pipe",
                ))),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;

        #[cfg(not(feature = "udp-timeout"))]
        let result = poll_fn(|cx| match self.recv_stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(msg)) => Poll::Ready(Ok(msg)),
            Poll::Ready(None) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Broken pipe",
            ))),
            Poll::Pending => Poll::Pending,
        })
        .await;

        #[cfg(feature = "udp-timeout")]
        if result.is_ok() {
            self.timeout
                .as_mut()
                .reset(Instant::now() + impl_inner::get_timeout_duration());
        }

        result
    }
}

/// [`UdpStream`] owned read-side implementation.
pub struct UdpStreamOwnedReadHalf {
    recv_stream: Pin<Box<ReceiveStreamOwned<Bytes>>>,
    remaining: Option<Bytes>,
    #[cfg(feature = "udp-timeout")]
    timeout: Pin<Box<Sleep>>,
    _guard: Arc<UdpStreamGuard>,
}

impl UdpStreamOwnedReadHalf {
    /// Receive a single UDP datagram as an owned buffer.
    pub async fn recv_datagram(&mut self) -> io::Result<Bytes> {
        if self.remaining.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "udp stream has buffered bytes; cannot recv datagram",
            ));
        }

        #[cfg(feature = "udp-timeout")]
        let result = poll_fn(|cx| {
            if self.timeout.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "UdpStream timeout with duration:{:?}",
                        impl_inner::get_timeout_duration()
                    ),
                )));
            }
            match self.recv_stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(msg)) => Poll::Ready(Ok(msg)),
                Poll::Ready(None) => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Broken pipe",
                ))),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;

        #[cfg(not(feature = "udp-timeout"))]
        let result = poll_fn(|cx| match self.recv_stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(msg)) => Poll::Ready(Ok(msg)),
            Poll::Ready(None) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Broken pipe",
            ))),
            Poll::Pending => Poll::Pending,
        })
        .await;

        #[cfg(feature = "udp-timeout")]
        if result.is_ok() {
            self.timeout
                .as_mut()
                .reset(Instant::now() + impl_inner::get_timeout_duration());
        }

        result
    }
}

/// [`UdpStream`] write-side implementation
pub struct UdpStreamWriteHalf<'a> {
    is_connect: bool,
    socket: &'a tokio::net::UdpSocket,
    peer_addr: SocketAddr,
}

impl UdpStreamWriteContext for std::pin::Pin<&mut UdpStreamWriteHalf<'_>> {
    fn get_socket(&self) -> &tokio::net::UdpSocket {
        self.socket
    }

    fn get_peer_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }

    fn is_connect(&self) -> bool {
        self.is_connect
    }
}

impl UdpStreamWriteContext for std::pin::Pin<&mut UdpStreamOwnedWriteHalf> {
    fn get_socket(&self) -> &tokio::net::UdpSocket {
        &self.socket
    }

    fn get_peer_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }

    fn is_connect(&self) -> bool {
        self.is_connect
    }
}

impl AsyncWrite for UdpStreamWriteHalf<'_> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        impl_inner::poll_write(self, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for UdpStreamOwnedWriteHalf {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        impl_inner::poll_write(self, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl UdpStreamWriteHalf<'_> {
    /// Send a single UDP datagram.
    pub async fn send_datagram(&self, data: &[u8]) -> io::Result<()> {
        let sent = if self.is_connect {
            self.socket.send(data).await?
        } else {
            self.socket.send_to(data, self.peer_addr).await?
        };
        if sent != data.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "udp datagram truncated",
            ));
        }
        Ok(())
    }
}

/// [`UdpStream`] owned write-side implementation.
pub struct UdpStreamOwnedWriteHalf {
    is_connect: bool,
    socket: Arc<tokio::net::UdpSocket>,
    peer_addr: SocketAddr,
    _guard: Arc<UdpStreamGuard>,
}

impl UdpStreamOwnedWriteHalf {
    /// Send a single UDP datagram.
    pub async fn send_datagram(&self, data: &[u8]) -> io::Result<()> {
        let sent = if self.is_connect {
            self.socket.send(data).await?
        } else {
            self.socket.send_to(data, self.peer_addr).await?
        };
        if sent != data.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "udp datagram truncated",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct UdpStreamGuard {
    _handler_guard: Option<TaskJoinHandleGuard>,
    _listener_guard: Option<ListenerCleanGuard>,
}
