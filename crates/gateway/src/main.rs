//! dolphone-gateway: Dolphone's texts over HTTP, for things that speak HTTP and not
//! D-Bus (a home-automation server, a monitoring system, a script on another box
//! through an SSH tunnel).
//!
//! It is optional and separate: phoned knows nothing about it, and treats it as the
//! outside program it is. Sending waits for a press on the device unless the owner
//! has given `dolphone-gateway` a standing grant for those numbers; reading needs
//! `dolphone-gateway` in `read_grants`. Every request needs the token in
//! `~/.config/dolphone/gateway.token` (made on first start, 0600), and it listens
//! on 127.0.0.1 unless told otherwise.
//!
//! ```text
//! GET  /v1/status                      the modem
//! POST /v1/messages {"to", "body"}     send (or wait for the device); {"id"} or {"pending"}
//! GET  /v1/threads                     conversations          (read grant)
//! GET  /v1/threads/<number>            one conversation       (read grant)
//! GET  /v1/events                      server-sent events: calls, texts (no bodies), alerts
//! ```

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use phone_api::{CallsProxy, MessagesProxy, ModemProxy};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A request may be no bigger than this.
const MAX_REQUEST: usize = 16 * 1024;

fn token_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"))
        .join("dolphone/gateway.token")
}

/// The token clients must send, made from the kernel's randomness on first start.
fn token() -> anyhow::Result<String> {
    use std::os::unix::fs::PermissionsExt;
    let path = token_path();
    if let Ok(t) = std::fs::read_to_string(&path) {
        if t.trim().len() >= 32 {
            return Ok(t.trim().to_string());
        }
    }
    let mut bytes = [0u8; 32];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut bytes)?;
    let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, &t)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
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

async fn handle(mut s: TcpStream, conn: zbus::Connection, token: String) -> anyhow::Result<()> {
    let req = match read_request(&mut s).await {
        Ok(r) => r,
        Err(_) => {
            reply(&mut s, 400, &json!({"error": "bad request"})).await;
            return Ok(());
        }
    };
    if !req.auth.as_deref().is_some_and(|a| same(a, &token)) {
        reply(&mut s, 401, &json!({"error": "a bearer token is required"})).await;
        return Ok(());
    }
    let messages = MessagesProxy::new(&conn).await?;
    let path = req.path.split('?').next().unwrap_or("");
    let result: Result<(u16, Value), zbus::Error> = match (req.method.as_str(), path) {
        ("GET", "/v1/status") => ModemProxy::new(&conn).await?.status().await.map(|st| (200, json!(st))),
        ("POST", "/v1/messages") => {
            let v: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
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
        ("GET", p) if p.starts_with("/v1/threads/") => {
            let peer = p.trim_start_matches("/v1/threads/").replace("%2B", "+");
            messages.conversation(&peer).await.map(|m| (200, json!(m)))
        }
        ("GET", "/v1/events") => return events(s, &conn).await,
        _ => Ok((404, json!({"error": "no such endpoint"}))),
    };
    match result {
        Ok((code, v)) => reply(&mut s, code, &v).await,
        Err(e) => {
            let (code, msg) = status_of(&e);
            reply(&mut s, code, &json!({"error": msg})).await;
        }
    }
    Ok(())
}

/// Server-sent events until the client goes away. Texts are announced without
/// their bodies, as phoned's own signals are.
async fn events(mut s: TcpStream, conn: &zbus::Connection) -> anyhow::Result<()> {
    let head =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    s.write_all(head.as_bytes()).await?;
    let calls = CallsProxy::new(conn).await?;
    let messages = MessagesProxy::new(conn).await?;
    let mut added = calls.receive_call_added().await?;
    let mut removed = calls.receive_call_removed().await?;
    let mut texts = messages.receive_message_added().await?;
    let mut alerts = messages.receive_alert_received().await?;
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--listen" => listen = it.next().context("--listen ADDR:PORT")?.clone(),
            "--print-token" => {
                println!("{}", token()?);
                return Ok(());
            }
            _ => {
                println!("dolphone-gateway [--listen 127.0.0.1:8475] [--print-token]");
                return Ok(());
            }
        }
    }
    let addr: std::net::SocketAddr = listen.parse().context("--listen ADDR:PORT")?;
    if !addr.ip().is_loopback() {
        tracing::warn!(%addr, "listening beyond this machine: anyone who can reach it and has the token can text as you");
    }
    let token = token()?;
    let conn = zbus::Connection::session().await.context("session bus")?;
    let listener = TcpListener::bind(addr).await.with_context(|| format!("listening on {addr}"))?;
    tracing::info!(%addr, "dolphone-gateway up");
    loop {
        let (s, _) = listener.accept().await?;
        let (conn, token) = (conn.clone(), token.clone());
        tokio::spawn(async move {
            if let Err(e) = handle(s, conn, token).await {
                tracing::debug!("request: {e}");
            }
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
