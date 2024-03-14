use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uni_stream::{
    addr::{get_ip_addrs, set_custom_dns_server, ToSocketAddrs},
    stream::{StreamProvider, TcpStreamProvider},
};

/// Udp connections are used in the same way as tcp connections using a customized dns server
async fn custom_dns_tcp_stream_read<A: ToSocketAddrs + Send>(addr: A) {
    // start tcp connect with custom dns resolver
    let mut stream = TcpStreamProvider::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /get HTTP/1.1\r\nHost: httpbin.org\r\nAccept: */*\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0; 1024];
    let n = stream.read(&mut buf).await.unwrap();
    println!("response:{}", String::from_utf8_lossy(&buf[..n]));
}

/// Get SocketAddr using a custom dns server
async fn custom_dns_resolver(host: &str) {
    println!("{host}:{:?}", get_ip_addrs(host).await.unwrap());
}

#[tokio::main]
async fn main() {
    // use google and alibaba dns server
    set_custom_dns_server(&["8.8.8.8".parse().unwrap(), "233.5.5.5".parse().unwrap()]).unwrap();
    // get http response
    custom_dns_tcp_stream_read("httpbin.org:80").await;
    // get bilibili.com addr
    custom_dns_resolver("bilibili.com").await;
}
