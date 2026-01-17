use tokio::io::Result;
use uni_stream::udp::UdpListener;

#[tokio::main]
async fn main() -> Result<()> {
    let listener = UdpListener::bind("127.0.0.1:9000").await?;
    println!("udp datagram echo server listening on 127.0.0.1:9000");

    loop {
        let (stream, addr) = listener.accept().await?;
        println!("accepted udp peer: {addr}");

        tokio::spawn(async move {
            let (mut reader, writer) = stream.split();
            loop {
                let msg = match reader.recv_datagram().await {
                    Ok(msg) => msg,
                    Err(err) => {
                        println!("recv_datagram error: {err}");
                        return;
                    }
                };
                if let Err(err) = writer.send_datagram(&msg).await {
                    println!("send_datagram error: {err}");
                    return;
                }
            }
        });
    }
}
