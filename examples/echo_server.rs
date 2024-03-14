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
