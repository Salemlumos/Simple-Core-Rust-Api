//! `api healthcheck`: consulta `/health/ready` da instância local. A imagem
//! distroless não tem shell nem curl.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::env_parse;

pub async fn run() -> anyhow::Result<()> {
    let bind: SocketAddr = env_parse("API_BIND", "127.0.0.1:8080".parse::<SocketAddr>()?)?;
    let ip = match bind.ip() {
        IpAddr::V4(v) if v.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v) if v.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let addr = SocketAddr::new(ip, bind.port());
    let check = async {
        let mut s = tokio::net::TcpStream::connect(addr).await?;
        s.write_all(b"GET /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut buf = Vec::with_capacity(512);
        s.read_to_end(&mut buf).await?;
        anyhow::Ok(buf)
    };
    let buf = tokio::time::timeout(Duration::from_secs(3), check).await??;
    let head = String::from_utf8_lossy(&buf[..buf.len().min(32)]).into_owned();
    if head.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        anyhow::bail!("não pronto: {}", head.lines().next().unwrap_or_default())
    }
}
