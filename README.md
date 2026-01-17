# uni-stream

[![crates.io](https://img.shields.io/crates/v/uni-stream.svg)](https://crates.io/crates/uni-stream)


`uni-stream` is a Rust library for unified operation of `TcpStream` and `UdpStream`, designed to make your service support UDP and TCP (such as proxy services) with a single code implementation. On top of that the library provides the ability to customize dns server resolution.

## Features

-   **Generic**: `uni-stream` provides an abstraction of UDP and TCP streams, which is exposed through some traits, making it easy for users to make secondary abstractions.
    
-   **Customizable**: `uni-stream` provides functions that allow users to customize the resolution of dns services for TCP or UDP connections.

-   **Datagram-first UDP API**: UDP is message-oriented. `uni-stream` exposes explicit
    `recv_datagram` / `send_datagram` APIs so callers can preserve UDP packet boundaries.

-   **Owned split for spawn**: `into_split()` returns owned read/write halves (`'static + Send`)
    for use in `tokio::spawn` or other cross-task workflows.
    
## Usage

To use `uni-stream` in your Rust project, simply add it as a dependency in your `Cargo.toml` file:

```toml
[dependencies]
uni-stream = "*"
``` 
You must also make sure that the Rust version >= 1.88 because some dependencies now require a
newer compiler baseline. Use the following `rust-toolchain.toml` in the project root directory:
```toml
[toolchain]
channel = "1.88.0"
```

Then, you can import and use the library in your Rust code.The following is a generic-based implementation of echo_server:

For UDP datagram-preserving forwarding, see `examples/udp_datagram_echo.rs`.

```rust,no_run
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use uni_stream::stream::ListenerProvider;
use uni_stream::stream::StreamAccept;
use uni_stream::stream::TcpListenerProvider;
use uni_stream::stream::UdpListenerProvider;

async fn echo_server<P: ListenerProvider>(
    server_addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = P::bind(server_addr).await?;
    println!("run local server:{server_addr}");
    loop {
        // Accept incoming connections
        let (mut stream, addr) = listener.accept().await?;
        println!("Connected from {addr}");

        // Process each connection concurrently
        tokio::spawn(async move {
            // Read data from client
            let mut buf = vec![0; 1024];
            loop {
                let n = match stream.read(&mut buf).await {
                    Ok(n) => n,
                    Err(e) => {
                        println!("Error reading: {e}");
                        return;
                    }
                };

                // If no data received, assume disconnect
                if n == 0 {
                    return;
                }

                // Echo data back to client
                if let Err(e) = stream.write_all(&buf[..n]).await {
                    println!("Error writing: {e}");
                    return;
                }

                println!("Echoed {n} bytes to {addr}");
            }
        });
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let run_udp: bool = true;
    if run_udp {
        echo_server::<UdpListenerProvider>("0.0.0.0:8080").await
    } else {
        echo_server::<TcpListenerProvider>("0.0.0.0:8080").await
    }
}
```

### Owned split (spawn-friendly)
When you need to move IO halves into a spawned task, use `into_split()` to get owned halves:
```rust,no_run
use uni_stream::stream::{StreamSplit, TcpStreamImpl};

async fn handle(stream: TcpStreamImpl) {
    let (mut reader, mut writer) = stream.into_split();
    tokio::spawn(async move {
        let _ = tokio::io::copy(&mut reader, &mut writer).await;
    });
}
```

### UDP datagram interface (important)

TCP is a byte stream; UDP is message-oriented. If you treat UDP like a byte stream, packet boundaries
are lost and higher-level protocols (e.g. QUIC, DNS, RTP, game traffic) can break.

`uni-stream` exposes explicit datagram APIs on UDP halves:

```text
UdpStreamReadHalf::recv_datagram()  -> returns exactly one UDP packet
UdpStreamWriteHalf::send_datagram() -> sends exactly one UDP packet
```

Simple UDP datagram echo (see `examples/udp_datagram_echo.rs`):

```rust,no_run
use std::error::Error;
use uni_stream::udp::UdpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let listener = UdpListener::bind("127.0.0.1:9000").await?;
    let (stream, _) = listener.accept().await?;
    let (mut reader, writer) = stream.split();
    let msg = reader.recv_datagram().await?;
    writer.send_datagram(&msg).await?;
    Ok(())
}
```

#### Why boundaries matter (short version)

- **TCP**: `read()` can return any number of bytes. It can merge or split packets.
- **UDP**: each `recv` returns exactly one datagram.

So a UDP tunnel must preserve packet boundaries; `uni-stream` provides the APIs to do that safely.

Visual intuition:

```text
TCP stream:
  bytes: [A][B][C] -> read() may return [A+B] or [B+C] or [A] then [B+C]

UDP datagrams:
  recv() returns exactly [A] then exactly [B] then exactly [C]
```

Customized dns resolution servers:
```rust,no_run
use uni_stream::addr::set_custom_dns_server;
// use google and alibaba dns server
set_custom_dns_server(&["8.8.8.8".parse().unwrap(), "233.5.5.5".parse().unwrap()]).unwrap();
```

Customize Udp Timeout:
```rust,no_run
use uni_stream::udp::set_custom_timeout;
use std::time::Duration;

let timeout = Duration::from_secs(5);
set_custom_timeout(timeout);
```
Or don't set any timeout on `UdpStream`
```toml
[dependencies]
uni-stream = { version = "0.1.0", default-features = false }
```



For more details on how to use `uni-stream`, please refer to the [examples](https://github.com/acking-you/uni-stream/tree/master/examples).

## Contributing

Contributions to `uni-stream` are welcome! If you would like to contribute to the library, please follow the standard Rust community guidelines for contributing, including opening issues, submitting pull requests, and providing feedback.

## License

`uni-stream` is licensed under the [MIT License](https://github.com/acking-you/uni-stream/blob/master/LICENSE), which allows for free use, modification, and distribution, subject to the terms and conditions outlined in the license.

We hope that `uni-stream` is useful for your projects! If you have any questions or need further assistance, please don't hesitate to contact us or open an issue in the repository.
