//! A tiny HTTP/1.1 server for adapter tests: records every request and answers from a
//! handler. One request per connection (`Connection: close`).

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Value,
}

type Handler = dyn Fn(&Req) -> (u16, Value) + Send + Sync;

pub struct Mock {
    pub url: String,
    pub reqs: Arc<Mutex<Vec<Req>>>,
}

impl Mock {
    pub fn requests(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }
    pub fn find(&self, method: &str, path: &str) -> Vec<Req> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == method && r.path == path)
            .collect()
    }
}

pub async fn serve(handler: impl Fn(&Req) -> (u16, Value) + Send + Sync + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let reqs: Arc<Mutex<Vec<Req>>> = Arc::default();
    let handler: Arc<Handler> = Arc::new(handler);
    let r2 = reqs.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let reqs = r2.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let mut lines = head.split("\r\n");
                let mut first = lines.next().unwrap_or("").split(' ');
                let method = first.next().unwrap_or("").to_string();
                let path = first.next().unwrap_or("").to_string();
                let headers: HashMap<String, String> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while buf.len() < header_end + len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body = serde_json::from_slice(&buf[header_end..]).unwrap_or(Value::Null);
                let req = Req {
                    method,
                    path,
                    headers,
                    body,
                };
                let (status, resp) = handler(&req);
                reqs.lock().unwrap().push(req);
                let text = if resp.is_null() {
                    String::new()
                } else {
                    resp.to_string()
                };
                let out = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Mock { url, reqs }
}
