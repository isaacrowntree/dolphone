//! dolphone-gateway: Dolphone's texts over HTTP, for things that speak HTTP and not
//! D-Bus (a home-automation server, a monitoring system, a script on another box
//! through an SSH tunnel).
//!
//! It is optional and separate: phoned knows nothing about it, and treats it as the
//! outside program it is. Sending waits for a press on the device unless the owner
//! has given `dolphone-gateway` a standing grant for those numbers; reading needs
//! `dolphone-gateway` in `read_grants`. Every request needs the token in
//! `~/.config/dolphone/gateway.token` (made on first start, 0600), and it listens
//! on 127.0.0.1 unless told otherwise. It speaks plain HTTP, so listening anywhere
//! else needs `--insecure-listen` as well: put TLS (or an SSH tunnel) in front.
//!
//! ```text
//! GET  /v1/status                      the modem
//! POST /v1/messages {"to", "body"}     send (or wait for the device); {"id"} or {"pending"}
//! GET  /v1/threads                     conversations          (read grant)
//! GET  /v1/threads/<number>            one conversation       (read grant)
//! GET  /v1/events                      server-sent events: calls, texts (no bodies), alerts
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use phone_api::{CallsProxy, MessagesProxy, ModemProxy};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

/// A request may be no bigger than this.
const MAX_REQUEST: usize = 16 * 1024;
/// The whole request must arrive in this long, however it trickles in; and the
/// answer must be ready in this long (an event stream aside, which lasts).
const READ_DEADLINE: Duration = Duration::from_secs(20);
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);
/// Connections served at once; more are closed as they come.
const MAX_CONNECTIONS: usize = 64;

fn token_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"))
        .join("dolphone/gateway.token")
}

/// The token clients must send, made from the kernel's randomness on first start.
fn token() -> anyhow::Result<String> {
    token_at(&token_path())
}

/// The token at `path`, made if it is missing. The directory is made 0700 and the
/// file 0600 as they are created, so the token is never readable by others, not
/// even for the moment between writing it and changing its mode.
fn token_at(path: &std::path::Path) -> anyhow::Result<String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    if let Ok(t) = std::fs::read_to_string(path) {
        if t.trim().len() >= 32 {
            return Ok(t.trim().to_string());
        }
    }
    let mut bytes = [0u8; 32];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut bytes)?;
    let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    // A file that was already there (a token too short to use) keeps its old
    // mode through open: set it before the new token goes in.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(t.as_bytes())?;
    eprintln!("dolphone-gateway: new token in {}", path.display());
    Ok(t)
}

/// Compare without stopping at the first difference, so timing says nothing.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Debug, PartialEq)]
struct Request {
    method: String,
    path: String,
    auth: Option<String>,
    body: Vec<u8>,
}

/// Read one HTTP/1.1 request, refusing anything oversized or slow.
async fn read_request(s: &mut TcpStream) -> anyhow::Result<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut chunk)).await??;
        anyhow::ensure!(n > 0, "closed");
        buf.extend_from_slice(&chunk[..n]);
        anyhow::ensure!(buf.len() <= MAX_REQUEST, "too big");
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
    };
    let head = std::str::from_utf8(&buf[..head_end])?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, path) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
    let mut len = 0usize;
    let mut auth = None;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => len = v.trim().parse()?,
            "authorization" => auth = v.trim().strip_prefix("Bearer ").map(str::to_string),
            _ => {}
        }
    }
    anyhow::ensure!(len <= MAX_REQUEST, "too big");
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut chunk)).await??;
        anyhow::ensure!(n > 0, "closed");
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Ok(Request { method, path, auth, body })
}

async fn reply(s: &mut TcpStream, status: u16, v: &Value) {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Internal Server Error",
    };
    let body = v.to_string();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes()).await;
    let _ = s.write_all(body.as_bytes()).await;
}

/// phoned's refusal as an HTTP status.
fn status_of(e: &zbus::Error) -> (u16, String) {
    let msg = match e {
        zbus::Error::MethodError(_, Some(m), _) => m.clone(),
        other => other.to_string(),
    };
    let name = match e {
        zbus::Error::MethodError(n, _, _) => n.as_str().to_string(),
        _ => String::new(),
    };
    let code = if name.ends_with("AccessDenied") {
        403
    } else if name.ends_with("LimitsExceeded") {
        429
    } else if name.ends_with("InvalidArgs") {
        400
    } else {
        500
    };
    (code, msg)
}

/// Decode %XX escapes in a path segment (a `+` stays a plus, as it does in a
/// path); None for a broken escape or text that is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

async fn handle(mut s: TcpStream, conn: zbus::Connection, token: String) -> anyhow::Result<()> {
    let read = tokio::time::timeout(READ_DEADLINE, read_request(&mut s)).await;
    let req = match read {
        Ok(Ok(r)) => r,
        _ => {
            reply(&mut s, 400, &json!({"error": "bad request"})).await;
            return Ok(());
        }
    };
    if !req.auth.as_deref().is_some_and(|a| same(a, &token)) {
        reply(&mut s, 401, &json!({"error": "a bearer token is required"})).await;
        return Ok(());
    }
    let path = req.path.split('?').next().unwrap_or("").to_string();
    if (req.method.as_str(), path.as_str()) == ("GET", "/v1/events") {
        return events(s, &conn).await;
    }
    match tokio::time::timeout(ANSWER_DEADLINE, answer(&conn, &req.method, &path, &req.body)).await {
        Ok(Ok((code, v))) => reply(&mut s, code, &v).await,
        Ok(Err(e)) => {
            let (code, msg) = status_of(&e);
            reply(&mut s, code, &json!({"error": msg})).await;
        }
        Err(_) => reply(&mut s, 504, &json!({"error": "phoned did not answer in time"})).await,
    }
    Ok(())
}

/// The answer to one request, but an event stream.
async fn answer(conn: &zbus::Connection, method: &str, path: &str, body: &[u8]) -> zbus::Result<(u16, Value)> {
    let unavailable = |e: zbus::Error| (503, json!({"error": format!("phoned is not reachable: {e}")}));
    let messages = match MessagesProxy::new(conn).await {
        Ok(m) => m,
        Err(e) => return Ok(unavailable(e)),
    };
    match (method, path) {
        ("GET", "/v1/status") => match ModemProxy::new(conn).await {
            Ok(m) => m.status().await.map(|st| (200, json!(st))),
            Err(e) => Ok(unavailable(e)),
        },
        ("POST", "/v1/messages") => {
            let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
            let (to, body) = (v["to"].as_str().unwrap_or(""), v["body"].as_str().unwrap_or(""));
            if to.is_empty() || body.is_empty() {
                Ok((400, json!({"error": "to and body are required"})))
            } else {
                messages.send(to, body).await.map(|p| {
                    if p.pending != 0 {
                        (202, json!({"pending": p.pending, "note": "waiting for a press on the device"}))
                    } else {
                        (200, json!({"id": p.id}))
                    }
                })
            }
        }
        ("GET", "/v1/threads") => messages.threads().await.map(|t| (200, json!(t))),
        ("GET", p) if p.starts_with("/v1/threads/") => match percent_decode(p.trim_start_matches("/v1/threads/")) {
            Some(peer) => messages.conversation(&peer).await.map(|m| (200, json!(m))),
            None => Ok((400, json!({"error": "the number is not escaped properly"}))),
        },
        _ => Ok((404, json!({"error": "no such endpoint"}))),
    }
}

/// Server-sent events until the client goes away. Texts are announced without
/// their bodies, as phoned's own signals are.
async fn events(mut s: TcpStream, conn: &zbus::Connection) -> anyhow::Result<()> {
    let streams = async {
        let calls = CallsProxy::new(conn).await?;
        let messages = MessagesProxy::new(conn).await?;
        zbus::Result::Ok((
            calls.receive_call_added().await?,
            calls.receive_call_removed().await?,
            messages.receive_message_added().await?,
            messages.receive_alert_received().await?,
        ))
    };
    let (mut added, mut removed, mut texts, mut alerts) = match tokio::time::timeout(ANSWER_DEADLINE, streams).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            reply(&mut s, 503, &json!({"error": format!("phoned is not reachable: {e}")})).await;
            return Ok(());
        }
        Err(_) => {
            reply(&mut s, 504, &json!({"error": "phoned did not answer in time"})).await;
            return Ok(());
        }
    };
    let head =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    s.write_all(head.as_bytes()).await?;
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    loop {
        let (kind, data) = tokio::select! {
            Some(e) = added.next() => ("call", json!(e.args()?.call)),
            Some(e) = removed.next() => ("call_ended", json!({"id": e.args()?.id})),
            Some(e) = texts.next() => ("message", json!(e.args()?.message)),
            Some(e) = alerts.next() => ("alert", json!(e.args()?.alert)),
            _ = ping.tick() => ("ping", json!({})),
        };
        if s.write_all(format!("event: {kind}\ndata: {data}\n\n").as_bytes()).await.is_err() {
            return Ok(());
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("dolphone_gateway=info").init();
    let mut listen = "127.0.0.1:8475".to_string();
    let mut insecure = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--listen" => listen = it.next().context("--listen ADDR:PORT")?.clone(),
            "--insecure-listen" => insecure = true,
            "--print-token" => {
                println!("{}", token()?);
                return Ok(());
            }
            _ => {
                println!("dolphone-gateway [--listen 127.0.0.1:8475 [--insecure-listen]] [--print-token]");
                return Ok(());
            }
        }
    }
    let addr: std::net::SocketAddr = listen.parse().context("--listen ADDR:PORT")?;
    if !addr.ip().is_loopback() {
        // Plain HTTP: the token, and every text read, cross the network in the
        // clear, for anyone on the path to copy.
        anyhow::ensure!(
            insecure,
            "refusing to listen on {addr}: the gateway speaks plain HTTP, so the token and your texts would \
             cross the network unencrypted. Listen on 127.0.0.1 and put TLS or an SSH tunnel in front, or \
             pass --insecure-listen to do it anyway"
        );
        tracing::warn!(%addr, "listening beyond this machine in plain HTTP: anyone on the path can read the token and your texts");
    }
    let token = token()?;
    let conn = zbus::Connection::session().await.context("session bus")?;
    let listener = TcpListener::bind(addr).await.with_context(|| format!("listening on {addr}"))?;
    tracing::info!(%addr, "dolphone-gateway up");
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut backoff = Duration::from_millis(50);
    loop {
        let s = match listener.accept().await {
            Ok((s, _)) => s,
            Err(e) => {
                // Out of file descriptors, most often: wait for some to close
                // rather than stop serving.
                tracing::warn!("accept: {e}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(2));
                continue;
            }
        };
        backoff = Duration::from_millis(50);
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            tracing::debug!("too many connections: one closed");
            continue;
        };
        let (conn, token) = (conn.clone(), token.clone());
        tokio::spawn(async move {
            if let Err(e) = handle(s, conn, token).await {
                tracing::debug!("request: {e}");
            }
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_whole() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
    }

    #[test]
    fn paths_are_percent_decoded() {
        assert_eq!(percent_decode("%2B61491570156").as_deref(), Some("+61491570156"));
        assert_eq!(percent_decode("+61%20491").as_deref(), Some("+61 491"));
        assert_eq!(percent_decode("%2b1").as_deref(), Some("+1"));
        assert_eq!(percent_decode("caf%C3%A9").as_deref(), Some("café"));
        assert_eq!(percent_decode("%2"), None, "a short escape");
        assert_eq!(percent_decode("%zz"), None, "not hex");
        assert_eq!(percent_decode("%FF"), None, "not UTF-8");
    }

    #[test]
    fn tokens_are_private_from_the_start() {
        use std::os::unix::fs::PermissionsExt;
        // No tempfile crate here: a directory of our own under the system's.
        let dir = std::env::temp_dir().join(format!("dolphone-gateway-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("dolphone/gateway.token");
        let t = token_at(&path).unwrap();
        assert_eq!(t.len(), 64);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(token_at(&path).unwrap(), t, "kept once made");
        // A short one left with a wide mode is replaced and narrowed.
        std::fs::write(&path, "short").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_ne!(token_at(&path).unwrap(), "short");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn requests_are_parsed_and_bounded() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut c = TcpStream::connect(addr).await.unwrap();
            c.write_all(
                b"POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer t0k\r\nContent-Length: 11\r\n\r\n{\"to\":\"1\"}x",
            )
            .await
            .unwrap();
        });
        let (mut s, _) = l.accept().await.unwrap();
        let r = read_request(&mut s).await.unwrap();
        client.await.unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/v1/messages");
        assert_eq!(r.auth.as_deref(), Some("t0k"));
        assert_eq!(r.body.len(), 11);

        let client = tokio::spawn(async move {
            let mut c = TcpStream::connect(addr).await.unwrap();
            let big = format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_REQUEST + 1);
            c.write_all(big.as_bytes()).await.unwrap();
        });
        let (mut s, _) = l.accept().await.unwrap();
        assert!(read_request(&mut s).await.is_err(), "oversized bodies are refused");
        client.await.unwrap();
    }
}
