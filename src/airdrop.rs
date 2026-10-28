//! A fake AirDrop receiver: Finder sees it as a nearby device and sends files to it.
//!
//! Protocol (as spoken by macOS 26 sharingd): HTTPS on AWDL, chunked request bodies,
//! `POST /Discover` -> name, `POST /Ask` -> accept, `POST /Upload` -> dvzip/cpio stream.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use socket2::{Domain, Socket, Type};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::archive::{self, CpioExtractor};

/// Called with the received files; resolves when the real device accepted them.
pub type Forward =
    Arc<dyn Fn(Vec<PathBuf>) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync>;

pub struct Receiver {
    /// Shown under the icon in Finder.
    pub name: String,
    pub model: String,
    pub spool: PathBuf,
    pub forward: Forward,
}

/// Lets a listener accept connections arriving over AWDL (SO_RECV_ANYIF).
const SO_RECV_ANYIF: libc::c_int = 0x1104;

pub fn bind() -> Result<TcpListener> {
    let s = Socket::new(Domain::IPV6, Type::STREAM, None)?;
    s.set_only_v6(true)?;
    let on: libc::c_int = 1;
    // SAFETY: setsockopt with a valid fd and an int-sized option value.
    let rc = unsafe {
        libc::setsockopt(
            std::os::fd::AsRawFd::as_raw_fd(&s),
            libc::SOL_SOCKET,
            SO_RECV_ANYIF,
            (&on as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error()).context("SO_RECV_ANYIF");
    }
    s.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)).into())?;
    s.listen(16)?;
    s.set_nonblocking(true)?;
    Ok(TcpListener::from_std(s.into())?)
}

pub async fn serve(listener: TcpListener, tls: TlsAcceptor, rx: Arc<Receiver>) {
    loop {
        let (tcp, from) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                log::warn!("accept: {e}");
                continue;
            }
        };
        // Serve only this Mac's Finder. AWDL reaches nearby Macs too, and their files would pass
        // through this Mac; refusing them also keeps the phone out of their AirDrop list.
        let own = from.ip().is_loopback() || matches!(from.ip(), IpAddr::V6(ip) if Some(ip) == crate::bonjour::awdl_address());
        if !own {
            log::debug!("ignoring AirDrop connection from another Mac at {from}");
            continue;
        }
        let (tls, rx) = (tls.clone(), rx.clone());
        tokio::spawn(async move {
            match tls.accept(tcp).await {
                Ok(stream) => {
                    if let Err(e) = handle(stream, &rx).await {
                        log::warn!("AirDrop connection from {from}: {e:#}");
                    }
                }
                Err(e) => log::debug!("TLS handshake from {from}: {e}"),
            }
        });
    }
}

struct Request {
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

async fn handle<S: AsyncRead + tokio::io::AsyncWrite + Unpin>(stream: S, rx: &Receiver) -> Result<()> {
    let mut conn = BufReader::new(stream);
    loop {
        let Some(req) = read_head(&mut conn).await? else { return Ok(()) };
        let mut body = Body::new(&req)?;
        log::debug!("{} {}", rx.name, req.path);
        let (status, reply) = match req.path.as_str() {
            "/Discover" => {
                body.drain(&mut conn).await?;
                (200, discover_reply(rx)?)
            }
            "/Ask" => {
                let ask = body.read_all(&mut conn).await?;
                log_ask(&ask, &rx.name);
                (200, plist_bytes(plist_dict(&[("ReceiverComputerName", &rx.name), ("ReceiverModelName", &rx.model)]))?)
            }
            "/Upload" => match upload(&req, &mut body, &mut conn, rx).await {
                Ok(()) => (200, Vec::new()),
                Err(e) => {
                    log::warn!("transfer to {} failed: {e:#}", rx.name);
                    // Finder shows "Failed"; the connection may hold unread body bytes.
                    respond(conn.get_mut(), 500, &[], true).await?;
                    return Ok(());
                }
            },
            _ => {
                body.drain(&mut conn).await?;
                (404, Vec::new())
            }
        };
        let close = req.header("Connection").is_some_and(|v| v.eq_ignore_ascii_case("close"));
        respond(conn.get_mut(), status, &reply, close).await?;
        if close {
            return Ok(());
        }
    }
}

async fn upload<R: AsyncBufRead + Unpin>(
    req: &Request,
    body: &mut Body,
    conn: &mut R,
    rx: &Receiver,
) -> Result<()> {
    let ctype = req.header("Content-Type").unwrap_or_default().to_ascii_lowercase();
    let id = req.header("TransferID").unwrap_or("upload").replace(['/', '.'], "_");
    let dir = rx.spool.join(id);
    let result = async {
        tokio::fs::create_dir_all(&dir).await?;
        let files = receive(&ctype, body, conn, &dir).await?;
        log::info!("received {} file(s) for {}, forwarding", files.len(), rx.name);
        (rx.forward)(files).await
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn receive<R: AsyncBufRead + Unpin>(
    ctype: &str,
    body: &mut Body,
    conn: &mut R,
    dir: &Path,
) -> Result<Vec<PathBuf>> {
    let mut cpio = CpioExtractor::new(dir);
    match ctype {
        "application/x-dvzip" => loop {
            let mut hdr = [0u8; 4];
            if !body.read_exact_or_eof(conn, &mut hdr).await? {
                break;
            }
            let (len, stored) = archive::dvzip_header(hdr)?;
            let mut payload = vec![0u8; len];
            if !body.read_exact_or_eof(conn, &mut payload).await? {
                bail!("upload ended mid-frame");
            }
            cpio.feed(&archive::dvzip_payload(&payload, stored)?)?;
        },
        "application/x-cpio" => {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = body.read(conn, &mut buf).await?;
                if n == 0 {
                    break;
                }
                cpio.feed(&buf[..n])?;
            }
        }
        other => bail!("unsupported upload type {other:?}"),
    }
    cpio.finish()
}

async fn read_head<R: AsyncBufRead + Unpin>(conn: &mut R) -> Result<Option<Request>> {
    let mut line = String::new();
    if conn.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(_method), Some(path)) = (parts.next(), parts.next()) else {
        bail!("bad request line {line:?}");
    };
    let path = path.to_string();
    let mut headers = Vec::new();
    loop {
        line.clear();
        if conn.read_line(&mut line).await? == 0 {
            bail!("connection closed in headers");
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if headers.len() > 100 {
            bail!("too many headers");
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok(Some(Request { path, headers }))
}

/// Request body reader for `Transfer-Encoding: chunked` or `Content-Length`.
enum Body {
    Chunked { left: u64, done: bool },
    Length(u64),
}

const MAX_SMALL_BODY: usize = 16 << 20;

impl Body {
    fn new(req: &Request) -> Result<Self> {
        if req.header("Transfer-Encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
            return Ok(Body::Chunked { left: 0, done: false });
        }
        let len = req.header("Content-Length").map(str::parse).transpose()?.unwrap_or(0);
        Ok(Body::Length(len))
    }

    async fn read<R: AsyncBufRead + Unpin>(&mut self, conn: &mut R, buf: &mut [u8]) -> Result<usize> {
        let left = match self {
            Body::Length(left) => left,
            Body::Chunked { done: true, .. } => return Ok(0),
            Body::Chunked { left, done } => {
                if *left == 0 {
                    let mut line = String::new();
                    conn.read_line(&mut line).await?;
                    let size = line.trim().split(';').next().unwrap_or_default();
                    *left = u64::from_str_radix(size, 16).with_context(|| format!("bad chunk size {line:?}"))?;
                    if *left == 0 {
                        *done = true;
                        // Trailers (normally none) end with an empty line.
                        loop {
                            line.clear();
                            if conn.read_line(&mut line).await? == 0 || line.trim().is_empty() {
                                break;
                            }
                        }
                        return Ok(0);
                    }
                }
                left
            }
        };
        if *left == 0 {
            return Ok(0);
        }
        let want = (*left).min(buf.len() as u64) as usize;
        let n = conn.read(&mut buf[..want]).await?;
        if n == 0 {
            bail!("connection closed mid-body");
        }
        *left -= n as u64;
        if *left == 0 && matches!(self, Body::Chunked { .. }) {
            let mut crlf = String::new();
            conn.read_line(&mut crlf).await?;
        }
        Ok(n)
    }

    /// Fill `buf` completely; false if the body ended before any byte was read.
    async fn read_exact_or_eof<R: AsyncBufRead + Unpin>(&mut self, conn: &mut R, buf: &mut [u8]) -> Result<bool> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = self.read(conn, &mut buf[filled..]).await?;
            if n == 0 {
                if filled == 0 {
                    return Ok(false);
                }
                bail!("body ended mid-record");
            }
            filled += n;
        }
        Ok(true)
    }

    async fn read_all<R: AsyncBufRead + Unpin>(&mut self, conn: &mut R) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = self.read(conn, &mut buf).await?;
            if n == 0 {
                return Ok(out);
            }
            if out.len() + n > MAX_SMALL_BODY {
                bail!("request body too large");
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    async fn drain<R: AsyncBufRead + Unpin>(&mut self, conn: &mut R) -> Result<()> {
        self.read_all(conn).await.map(drop)
    }
}

async fn respond<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, status: u16, body: &[u8], close: bool) -> Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n",
        body.len(),
        if close { "close" } else { "keep-alive" }
    );
    w.write_all(head.as_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

fn plist_dict(pairs: &[(&str, &str)]) -> plist::Dictionary {
    pairs.iter().map(|(k, v)| (k.to_string(), plist::Value::String(v.to_string()))).collect()
}

fn plist_bytes(dict: plist::Dictionary) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    plist::Value::Dictionary(dict).to_writer_binary(&mut out)?;
    Ok(out)
}

fn discover_reply(rx: &Receiver) -> Result<Vec<u8>> {
    let mut d = plist_dict(&[("ReceiverComputerName", &rx.name), ("ReceiverModelName", &rx.model)]);
    // No codec support advertised, so senders convert HEIC/HEVC to JPEG/H.264 for Android.
    d.insert("ReceiverMediaCapabilities".into(), plist::Value::Data(br#"{"Version":1}"#.to_vec()));
    plist_bytes(d)
}

fn log_ask(body: &[u8], to: &str) {
    let Ok(plist::Value::Dictionary(d)) = plist::from_bytes::<plist::Value>(body) else { return };
    let from = d.get("SenderComputerName").and_then(|v| v.as_string()).unwrap_or("?");
    let files = d.get("Files").and_then(|v| v.as_array()).map_or(0, Vec::len);
    log::info!("{from} wants to send {files} file(s) to {to}");
}
