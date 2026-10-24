//! A tiny HTTP server for the retrieval tests: answers from a route table and records
//! every request line, so tests can check what was requested and in what order.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;

/// What the server answers for a path.
#[derive(Clone)]
pub enum Reply {
    Body(Vec<u8>),
    Status(u16),
    Redirect(String),
    /// Any status, extra header lines and body.
    Raw(u16, Vec<(String, String)>, Vec<u8>),
    /// A body announced as `declared` bytes long, of which only these are sent before
    /// the connection is closed.
    Truncated(Vec<u8>, usize),
    /// Waits before sending anything.
    Slow(Duration),
    /// Sends the headers of a longer body, then nothing more.
    Stall(Duration),
}

pub fn body(value: &Value) -> Reply {
    Reply::Body(value.to_string().into_bytes())
}

pub struct Server {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub fn start(routes: Vec<(&'static str, Reply)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port is free");
        let base = format!("http://{}", listener.local_addr().expect("bound"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<HashMap<&str, Reply>> = Arc::new(routes.into_iter().collect());
        let seen = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let routes = Arc::clone(&routes);
                let seen = Arc::clone(&seen);
                thread::spawn(move || serve(stream, &routes, &seen));
            }
        });
        Self { base, requests }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// `host:port`.
    pub fn authority(&self) -> &str {
        &self.base["http://".len()..]
    }

    /// The request targets so far, in order (`/path?query`, or the absolute URL when a
    /// client speaks to a proxy).
    pub fn hits(&self) -> Vec<String> {
        self.request_lines()
            .iter()
            .map(|line| line.split_whitespace().nth(1).unwrap_or("").to_owned())
            .collect()
    }

    /// The request lines so far (`GET /path HTTP/1.1`).
    pub fn request_lines(&self) -> Vec<String> {
        self.requests.lock().expect("not poisoned").clone()
    }
}

fn serve(mut stream: TcpStream, routes: &HashMap<&str, Reply>, seen: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok() && line != "\r\n" && !line.is_empty() {
        line.clear();
    }
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_owned();
    seen.lock()
        .expect("not poisoned")
        .push(request_line.trim_end().to_owned());
    // As a proxy: tunnel `CONNECT host:port` to that host (this server, in the tests),
    // whose own handler records the request that follows.
    if request_line.starts_with("CONNECT ") {
        let Ok(upstream) = TcpStream::connect(&target) else {
            let _ = write!(
                stream,
                "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n"
            );
            return;
        };
        let _ = write!(stream, "HTTP/1.1 200 Connection established\r\n\r\n");
        let mut client_in = stream.try_clone().expect("clone");
        let mut client_out = stream;
        let mut upstream_in = upstream.try_clone().expect("clone");
        let mut upstream_out = upstream;
        thread::spawn(move || {
            let _ = std::io::copy(&mut client_in, &mut upstream_out);
            let _ = upstream_out.shutdown(std::net::Shutdown::Write);
        });
        let _ = std::io::copy(&mut upstream_in, &mut client_out);
        let _ = client_out.shutdown(std::net::Shutdown::Write);
        return;
    }
    // A proxied request carries the absolute URL: route on its path.
    let path = target
        .strip_prefix("http://")
        .and_then(|rest| rest.find('/').map(|i| &rest[i..]))
        .unwrap_or(&target);
    let path = path.split('?').next().unwrap_or("");
    let (status, extra, payload): (u16, String, Vec<u8>) = match routes.get(path) {
        None => (404, String::new(), b"<html>nope</html>".to_vec()),
        Some(Reply::Body(bytes)) => (200, String::new(), bytes.clone()),
        Some(Reply::Status(status)) => (*status, String::new(), b"boom".to_vec()),
        Some(Reply::Redirect(to)) => (302, format!("Location: {to}\r\n"), Vec::new()),
        Some(Reply::Raw(status, headers, bytes)) => (
            *status,
            headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}\r\n"))
                .collect(),
            bytes.clone(),
        ),
        Some(Reply::Truncated(bytes, declared)) => {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(bytes);
            return;
        }
        Some(Reply::Slow(delay)) => {
            thread::sleep(*delay);
            (200, String::new(), b"{}".to_vec())
        }
        Some(Reply::Stall(delay)) => {
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{{");
            let _ = stream.flush();
            thread::sleep(*delay);
            return;
        }
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        payload.len()
    );
    let _ = stream.write_all(&payload);
}
