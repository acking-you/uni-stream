use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uni_stream::{
    addr::ToSocketAddrs,
    stream::{StreamProvider, TcpStreamProvider, UdpStreamProvider},
};

struct TimerTickGurad {
    ins: Instant,
}

impl TimerTickGurad {
    fn new() -> Self {
        Self {
            ins: Instant::now(),
        }
    }
}

impl Drop for TimerTickGurad {
    fn drop(&mut self) {
        let end = Instant::now();
        let duration = end - self.ins;
        println!("duration:{duration:?}");
    }
}

/// test echo delay
async fn echo_delay<P: StreamProvider, A: ToSocketAddrs + 'static + Send>(addr: A) {
    let mut stream = P::connect(addr).await.unwrap();
    let mut buf = [0; 1024];
    let expected = b"abc";
    for _ in 0..10 {
        let n = {
            let _guard = TimerTickGurad::new();
            stream.write_all(expected).await.unwrap();
            stream.read(&mut buf).await.unwrap()
        };

        assert_eq!(expected, &buf[..n]);
    }
}

#[tokio::main]
async fn main() {
    let is_udp = true;
    if is_udp {
        echo_delay::<UdpStreamProvider, _>("127.0.0.1:8080").await
    } else {
        echo_delay::<TcpStreamProvider, _>("127.0.0.1:8080").await
    }
}
