//! The one HTTP client ietsp has: a GET that returns the bytes, for flake
//! inputs (`flake::fetch`). hyper over tokio-rustls, the stack tonic already
//! brings; no libcurl. Redirects are followed like
//! filetransfer.cc (`CURLOPT_FOLLOWLOCATION`, `CURLOPT_MAXREDIRS` 10) and
//! `file://` reads the local file, as libcurl does for the `tarball` inputs
//! `nix flake lock` writes with that scheme.

use std::io::Read;
use std::sync::{Arc, OnceLock};

use crate::error::{Error, Result};

/// filetransfer.cc:CURLOPT_MAXREDIRS.
const MAX_REDIRECTS: usize = 10;

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

/// `GET url` with `headers`, following redirects; the response body on 200.
/// A `file://` URL is read from the file system instead.
pub fn get(url: &str, headers: &[(String, String)]) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        let path =
            crate::remote::worker::percent_decode(path.split(['?', '#']).next().unwrap_or(path));
        return std::fs::read(&path).map_err(|e| Error::io(format!("cannot read '{path}'"), e));
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::io("cannot start a tokio runtime", e))?;
    rt.block_on(get_async(url, headers))
}

async fn get_async(url: &str, headers: &[(String, String)]) -> Result<Vec<u8>> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let (status, location, body) = once(&current, headers).await?;
        match status {
            200 => return Ok(body),
            301 | 302 | 303 | 307 | 308 => {
                let Some(to) = location else {
                    return Err(bad(&current, &format!("HTTP {status} with no Location")));
                };
                current = resolve(&current, &to);
            }
            other => {
                return Err(bad(&current, &format!("HTTP status {other}")));
            }
        }
    }
    Err(bad(url, &format!("more than {MAX_REDIRECTS} redirects")))
}

/// One request: the status, the `Location` header and the body.
async fn once(url: &str, headers: &[(String, String)]) -> Result<(u16, Option<String>, Vec<u8>)> {
    use http_body_util::BodyExt;

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
    let body = res
        .into_body()
        .collect()
        .await
        .map_err(|e| bad(url, &format!("reading the response failed: {e}")))?
        .to_bytes()
        .to_vec();
    driver.abort();
    Ok((status, location, body))
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

/// `bytes` with the compression it announces undone.
pub fn decompress(bytes: Vec<u8>) -> Result<Vec<u8>> {
    let read = |mut r: Box<dyn Read>, what: &str| -> Result<Vec<u8>> {
        let mut out = Vec::new();
        r.read_to_end(&mut out)
            .map_err(|e| Error::io(format!("cannot decompress the {what} stream"), e))?;
        Ok(out)
    };
    let cur = std::io::Cursor::new(bytes);
    match Compression::sniff(cur.get_ref()) {
        Compression::None => Ok(cur.into_inner()),
        Compression::Gzip => read(Box::new(flate2::read::MultiGzDecoder::new(cur)), "gzip"),
        Compression::Xz => read(Box::new(lzma_rust2::XzReader::new(cur, true)), "xz"),
        Compression::Zstd => read(
            Box::new(
                zstd::stream::read::Decoder::new(cur)
                    .map_err(|e| Error::io("cannot start the zstd decoder", e))?,
            ),
            "zstd",
        ),
        Compression::Bzip2 => read(Box::new(bzip2::read::MultiBzDecoder::new(cur)), "bzip2"),
    }
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
