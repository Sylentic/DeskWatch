//! A very small HTTPS client for the spike: parse the address, send one GET,
//! count what comes back. Not a general HTTP client.

use embassy_time::Instant;
use heapless::Vec;
use mbedtls_rs::{Session, SessionError};

/// `https://host[:port]/path`, split up. IPv6 literals and credentials in the
/// address are not supported.
pub struct Url<'a> {
    pub host: &'a str,
    pub port: u16,
    pub path: &'a str,
}

impl<'a> Url<'a> {
    pub fn parse(url: &'a str) -> Result<Self, &'static str> {
        let rest = url
            .strip_prefix("https://")
            .ok_or("only https:// addresses are supported")?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().map_err(|_| "bad port number")?),
            None => (authority, 443),
        };
        if host.is_empty() || host.contains('@') {
            return Err("missing or unsupported host name");
        }
        Ok(Self { host, port, path })
    }
}

/// How much of the response start is kept to find the end of the headers and
/// to print them.
const HEAD_KEEP: usize = 1024;

/// Counters for one response.
pub struct Reader {
    pub status: u16,
    pub total: usize,
    pub first_byte_ms: u64,
    head: Vec<u8, HEAD_KEEP>,
}

impl Reader {
    pub fn new() -> Self {
        Self {
            status: 0,
            total: 0,
            first_byte_ms: 0,
            head: Vec::new(),
        }
    }

    fn feed(&mut self, chunk: &[u8], since: &Instant) {
        if self.total == 0 {
            self.first_byte_ms = since.elapsed().as_millis();
        }
        self.total += chunk.len();
        let room = HEAD_KEEP - self.head.len();
        let _ = self.head.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if self.status == 0
            && let Some(code) = self.head.strip_prefix(b"HTTP/1.").and_then(|r| r.get(2..5))
        {
            self.status = core::str::from_utf8(code)
                .ok()
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
        }
    }

    /// Offset of the first body byte, if the whole header block was kept.
    fn header_len(&self) -> Option<usize> {
        self.head
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
    }

    /// Bytes after the headers (everything, if the header end was not seen).
    pub fn body_bytes(&self) -> usize {
        self.total - self.header_len().unwrap_or(0)
    }

    /// The status line and headers as text, for the log.
    pub fn head_lines(&self) -> Option<&str> {
        let end = self.header_len().unwrap_or(self.head.len());
        core::str::from_utf8(&self.head[..end])
            .ok()
            .map(str::trim_end)
    }
}

/// Send `GET` and read until the server closes the connection.
pub async fn get<T>(
    session: &mut Session<'_, T>,
    url: &Url<'_>,
    reader: &mut Reader,
    started: &Instant,
) -> Result<(), SessionError>
where
    T: embedded_io_async::Read + embedded_io_async::Write,
{
    // HTTP/1.1 with "Connection: close" so the end of the body is the end of the
    // stream, and no compression so nothing needs unpacking.
    for part in [
        "GET ",
        url.path,
        " HTTP/1.1\r\nHost: ",
        url.host,
        "\r\nUser-Agent: deskwatch-tls-spike\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
    ] {
        let mut data = part.as_bytes();
        while !data.is_empty() {
            let n = session.write(data).await?;
            data = &data[n..];
        }
    }
    session.flush().await?;

    let mut buf = [0u8; 1024];
    loop {
        let n = session.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        reader.feed(&buf[..n], started);
    }
}
