//! The one HTTP client ietsp has: a streaming GET, for flake inputs
//! (`flake::fetch`). hyper over tokio-rustls, the stack tonic already
//! brings; no libcurl. Redirects are followed like
//! filetransfer.cc (`CURLOPT_FOLLOWLOCATION`, `CURLOPT_MAXREDIRS` 10) and
//! `file://` reads the local file, as libcurl does for the `tarball` inputs
//! `nix flake lock` writes with that scheme.
//!
//! The body is never held whole: hyper runs on a thread of its own and hands
//! its frames to a bounded queue, the caller's thread reads that queue through
//! the decompressor, and both ends stop when the other does.

use std::io::{Read, Write};
use std::sync::{Arc, OnceLock};

use crate::error::{Error, Result};

/// filetransfer.cc:CURLOPT_MAXREDIRS.
const MAX_REDIRECTS: usize = 10;

/// Body frames in flight between hyper and the reader. hyper's frames are tens
/// of kilobytes, so this is the download's whole memory cost.
const QUEUE: usize = 16;

/// What a URL was split into; `target` is the request line's path and query.
struct Url {
    https: bool,
    host: String,
    port: u16,
    target: String,
}

fn bad(url: &str, why: &str) -> Error {
    Error::Usage(format!("cannot fetch '{url}': {why}"))
}

impl Url {
    fn parse(url: &str) -> Result<Url> {
        let (scheme, rest) = url.split_once("://").ok_or_else(|| bad(url, "no scheme"))?;
        let https = match scheme {
            "https" => true,
            "http" => false,
            other => return Err(bad(url, &format!("unsupported scheme '{other}'"))),
        };
        let (authority, path) = match rest.find(['/', '?', '#']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                (h, p.parse::<u16>().map_err(|_| bad(url, "bad port"))?)
            }
            _ => (authority, if https { 443 } else { 80 }),
        };
        if host.is_empty() {
            return Err(bad(url, "no host"));
        }
        Ok(Url {
            https,
            host: host.to_string(),
            port,
            target: if path.is_empty() {
                "/".to_string()
            } else {
                path.to_string()
            },
        })
    }

    fn authority(&self) -> String {
        let default = if self.https { 443 } else { 80 };
        if self.port == default {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// `Location` against the URL it came from: absolute, host-relative or
/// path-relative (libcurl resolves all three).
fn resolve(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if location.starts_with('/') {
        return format!("{scheme}://{authority}{location}");
    }
    let path = &rest[authority.len()..];
    let dir = path.rsplit_once('/').map_or("/", |(d, _)| d);
    format!("{scheme}://{authority}{dir}/{location}")
}

/// One rustls client config for the process: the platform's CA store, which is
/// what curl uses too.
fn tls() -> Result<Arc<rustls::ClientConfig>> {
    static TLS: OnceLock<std::result::Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    TLS.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        let found = rustls_native_certs::load_native_certs();
        if found.certs.is_empty() {
            return Err(format!(
                "no trusted CA certificates found: {}",
                found
                    .errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        for c in found.certs {
            let _ = roots.add(c);
        }
        let mut cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(cfg))
    })
    .clone()
    .map_err(Error::Config)
}

/// `GET url` with `headers`, following redirects, with the body decompressed
/// into `sink` as it arrives; the number of bytes written. A `file://` URL is
/// read from the file system instead. Nothing buffers the whole body.
pub fn get_decompressed(
    url: &str,
    headers: &[(String, String)],
    sink: &mut dyn Write,
) -> Result<u64> {
    if let Some(path) = url.strip_prefix("file://") {
        let path =
            crate::remote::worker::percent_decode(path.split(['?', '#']).next().unwrap_or(path));
        let file = std::fs::File::open(&path)
            .map_err(|e| Error::io(format!("cannot read '{path}'"), e))?;
        return drain(decompress_reader(Box::new(file))?, sink);
    }
    let (tx, rx) = tokio::sync::mpsc::channel(QUEUE);
    let target = url.to_string();
    let sent = headers.to_vec();
    let download = std::thread::spawn(move || -> Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::io("cannot start a tokio runtime", e))?;
        rt.block_on(stream(&target, &sent, tx))
    });
    let read = decompress_reader(Box::new(Queued {
        rx,
        at: Vec::new(),
        pos: 0,
    }))
    .and_then(|r| drain(r, sink));
    download
        .join()
        .map_err(|_| Error::Store("the download thread panicked".into()))??;
    read
}

/// The reading half of the queue: hyper's frames, one after the other.
struct Queued {
    rx: tokio::sync::mpsc::Receiver<std::result::Result<Vec<u8>, String>>,
    at: Vec<u8>,
    pos: usize,
}

impl Read for Queued {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pos == self.at.len() {
            match self.rx.blocking_recv() {
                None => return Ok(0),
                Some(Err(e)) => return Err(std::io::Error::other(e)),
                Some(Ok(chunk)) => {
                    self.at = chunk;
                    self.pos = 0;
                }
            }
        }
        let n = buf.len().min(self.at.len() - self.pos);
        buf[..n].copy_from_slice(&self.at[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn drain(mut read: Box<dyn Read>, sink: &mut dyn Write) -> Result<u64> {
    std::io::copy(&mut read, sink).map_err(|e| Error::io("cannot read the download", e))
}

/// Follows redirects and pushes the frames of the 200 into `tx`.
async fn stream(
    url: &str,
    headers: &[(String, String)],
    tx: tokio::sync::mpsc::Sender<std::result::Result<Vec<u8>, String>>,
) -> Result<()> {
    use http_body_util::BodyExt;

    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let (status, location, mut body, driver) = once(&current, headers).await?;
        match status {
            200 => {
                let mut sent = Ok(());
                while let Some(frame) = body.frame().await {
                    let frame = match frame {
                        Ok(f) => f,
                        Err(e) => {
                            let why = format!("reading the response failed: {e}");
                            let _ = tx.send(Err(bad(&current, &why).to_string())).await;
                            sent = Err(bad(&current, &why));
                            break;
                        }
                    };
                    if let Ok(data) = frame.into_data()
                        && tx.send(Ok(data.to_vec())).await.is_err()
                    {
                        break;
                    }
                }
                driver.abort();
                return sent;
            }
            301 | 302 | 303 | 307 | 308 => {
                let _ = body.collect().await;
                driver.abort();
                let Some(to) = location else {
                    return Err(bad(&current, &format!("HTTP {status} with no Location")));
                };
                current = resolve(&current, &to);
            }
            other => {
                driver.abort();
                return Err(bad(&current, &format!("HTTP status {other}")));
            }
        }
    }
    Err(bad(url, &format!("more than {MAX_REDIRECTS} redirects")))
}

type Started = (
    u16,
    Option<String>,
    hyper::body::Incoming,
    tokio::task::JoinHandle<std::result::Result<(), hyper::Error>>,
);

/// One request: the status, the `Location` header, the body not yet read, and
/// the connection task that must live as long as the body.
async fn once(url: &str, headers: &[(String, String)]) -> Result<Started> {
    let u = Url::parse(url)?;
    let stream = tokio::net::TcpStream::connect((u.host.as_str(), u.port))
        .await
        .map_err(|e| Error::io(format!("cannot connect to '{}'", u.authority()), e))?;
    let io: Box<dyn Stream> = if u.https {
        let name = rustls::pki_types::ServerName::try_from(u.host.clone())
            .map_err(|_| bad(url, "the host is not a valid TLS name"))?;
        let tls = tokio_rustls::TlsConnector::from(tls()?);
        Box::new(
            tls.connect(name, stream)
                .await
                .map_err(|e| Error::io(format!("TLS handshake with '{}'", u.host), e))?,
        )
    } else {
        Box::new(stream)
    };
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io))
            .await
            .map_err(|e| bad(url, &format!("HTTP handshake failed: {e}")))?;
    let driver = tokio::spawn(conn);

    let mut req = hyper::Request::builder()
        .method("GET")
        .uri(&u.target)
        .header("host", u.authority())
        .header("user-agent", concat!("ietsp/", env!("CARGO_PKG_VERSION")));
    for (name, value) in headers {
        req = req.header(name.as_str(), value.as_str());
    }
    let req = req
        .body(String::new())
        .map_err(|e| bad(url, &format!("bad request: {e}")))?;
    let res = sender
        .send_request(req)
        .await
        .map_err(|e| bad(url, &format!("request failed: {e}")))?;
    let status = res.status().as_u16();
    let location = res
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    Ok((status, location, res.into_body(), driver))
}

trait Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> Stream for T {}

/// The magic bytes libarchive's `archive_read_support_filter_all` recognises,
/// for the filters ietsp implements (tarfile.cc:TarArchive).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Compression {
    None,
    Gzip,
    Xz,
    Zstd,
    Bzip2,
}

impl Compression {
    pub fn sniff(bytes: &[u8]) -> Compression {
        const XZ: &[u8] = &[0xfd, b'7', b'z', b'X', b'Z', 0x00];
        const ZSTD: &[u8] = &[0x28, 0xb5, 0x2f, 0xfd];
        if bytes.starts_with(&[0x1f, 0x8b]) {
            Compression::Gzip
        } else if bytes.starts_with(XZ) {
            Compression::Xz
        } else if bytes.starts_with(ZSTD) {
            Compression::Zstd
        } else if bytes.starts_with(b"BZh") {
            Compression::Bzip2
        } else {
            Compression::None
        }
    }
}

/// The magic bytes read ahead of the stream they came from.
struct Sniffed {
    head: Vec<u8>,
    pos: usize,
    rest: Box<dyn Read>,
}

impl Read for Sniffed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos < self.head.len() {
            let n = buf.len().min(self.head.len() - self.pos);
            buf[..n].copy_from_slice(&self.head[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        self.rest.read(buf)
    }
}

/// `read` with the compression its first bytes announce undone, as
/// `archive_read_support_filter_all` does it, without holding the stream.
pub fn decompress_reader(mut read: Box<dyn Read>) -> Result<Box<dyn Read>> {
    let mut head = [0u8; 6];
    let mut n = 0;
    while n < head.len() {
        match read.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::io("cannot read the archive", e)),
        }
    }
    let kind = Compression::sniff(&head[..n]);
    let src = Sniffed {
        head: head[..n].to_vec(),
        pos: 0,
        rest: read,
    };
    Ok(match kind {
        Compression::None => Box::new(src),
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(src)),
        Compression::Xz => Box::new(lzma_rust2::XzReader::new(src, true)),
        Compression::Zstd => Box::new(
            zstd::stream::read::Decoder::new(src)
                .map_err(|e| Error::io("cannot start the zstd decoder", e))?,
        ),
        Compression::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(src)),
    })
}

/// `bytes` with the compression it announces undone.
pub fn decompress(bytes: Vec<u8>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    decompress_reader(Box::new(std::io::Cursor::new(bytes)))?
        .read_to_end(&mut out)
        .map_err(|e| Error::io("cannot decompress the stream", e))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split() {
        let u = Url::parse("https://api.github.com/repos/a/b/tarball/c").unwrap();
        assert!(u.https);
        assert_eq!((u.host.as_str(), u.port), ("api.github.com", 443));
        assert_eq!(u.target, "/repos/a/b/tarball/c");
        assert_eq!(u.authority(), "api.github.com");

        let u = Url::parse("http://localhost:8080").unwrap();
        assert!(!u.https);
        assert_eq!(
            (u.host.as_str(), u.port, u.target.as_str()),
            ("localhost", 8080, "/")
        );
        assert_eq!(u.authority(), "localhost:8080");

        let u =
            Url::parse("https://gitlab.com/api/v4/projects/a%2Fb/repository/archive.tar.gz?sha=d")
                .unwrap();
        assert_eq!(
            u.target,
            "/api/v4/projects/a%2Fb/repository/archive.tar.gz?sha=d"
        );

        assert!(Url::parse("ftp://x/y").is_err());
        assert!(Url::parse("nonsense").is_err());
    }

    #[test]
    fn redirects_resolve() {
        assert_eq!(
            resolve(
                "https://api.github.com/repos/a/b/tarball/c",
                "https://codeload.github.com/a/b/tar.gz/c"
            ),
            "https://codeload.github.com/a/b/tar.gz/c"
        );
        assert_eq!(
            resolve("https://x.example/a/b?q=1", "/c/d"),
            "https://x.example/c/d"
        );
        assert_eq!(
            resolve("https://x.example/a/b", "c"),
            "https://x.example/a/c"
        );
    }

    #[test]
    fn magic_bytes() {
        assert_eq!(Compression::sniff(&[0x1f, 0x8b, 8, 0]), Compression::Gzip);
        assert_eq!(Compression::sniff(b"BZh9"), Compression::Bzip2);
        assert_eq!(
            Compression::sniff(&[0xfd, b'7', b'z', b'X', b'Z', 0]),
            Compression::Xz
        );
        assert_eq!(
            Compression::sniff(&[0x28, 0xb5, 0x2f, 0xfd]),
            Compression::Zstd
        );
        assert_eq!(Compression::sniff(b"./file\0\0"), Compression::None);
    }
}
