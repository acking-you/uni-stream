# uni-stream

[![crates.io](https://img.shields.io/crates/v/uni-stream.svg)](https://crates.io/crates/uni-stream)


`uni-stream` is a Rust library for unified operation of `TcpStream` and `UdpStream`, designed to make your service support UDP and TCP (such as proxy services) with a single code implementation. On top of that the library provides the ability to customize dns server resolution.

## Features

-   **Generic**: `uni-stream` provides an abstraction of UDP and TCP streams, which is exposed through some traits, making it easy for users to make secondary abstractions.
    
-   **Customizable**: `uni-stream` provides functions that allow users to customize the resolution of dns services for TCP or UDP connections.
    
## Usage

To use `uni-stream` in your Rust project, simply add it as a dependency in your `Cargo.toml` file:

```toml
[dependencies]
uni-stream = "0.0.1"
``` 
You must also make sure that the Rust version >= 1.75, because [AFIT](https://blog.rust-lang.org/2023/12/28/Rust-1.75.0.html) is required, so you need to add the following `rust-toolchain.toml` to the project root directory:
```toml
[toolchain]
channel = "1.75.0"
```

Then, you can import and use the library in your Rust code.The following is a generic-based implementation of echo_server:

```rust
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
        println!("Connected from {}", addr);

        // Process each connection concurrently
        tokio::spawn(async move {
            // Read data from client
            let mut buf = vec![0; 1024];
            loop {
                let n = match stream.read(&mut buf).await {
                    Ok(n) => n,
                    Err(e) => {
                        println!("Error reading: {}", e);
                        return;
                    }
                };

                // If no data received, assume disconnect
                if n == 0 {
                    return;
                }

                // Echo data back to client
                if let Err(e) = stream.write_all(&buf[..n]).await {
                    println!("Error writing: {}", e);
                    return;
                }

                println!("Echoed {} bytes to {}", n, addr);
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

Customized dns resolution servers:
```rust
use uni_stream::addr::set_custom_dns_server;
// use google and alibaba dns server
set_custom_dns_server(&["8.8.8.8".parse().unwrap(), "233.5.5.5".parse().unwrap()]).unwrap();
```


For more details on how to use `uni-stream`, please refer to the [examples](https://github.com/acking-you/uni-stream/tree/master/examples).

## Contributing

Contributions to `uni-stream` are welcome! If you would like to contribute to the library, please follow the standard Rust community guidelines for contributing, including opening issues, submitting pull requests, and providing feedback.

## License

`uni-stream` is licensed under the [MIT License](https://github.com/acking-you/uni-stream/blob/master/LICENSE), which allows for free use, modification, and distribution, subject to the terms and conditions outlined in the license.

We hope that `uni-stream` is useful for your projects! If you have any questions or need further assistance, please don't hesitate to contact us or open an issue in the repository.