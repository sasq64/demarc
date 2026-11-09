//! A local web server for web releases: pages opened from `file://` have a null
//! origin, and some refuse to run that way ("Browser security forbids loading
//! files directly without a webserver").
//!
//! One server for the whole run, started on first use. Each release directory
//! gets a number, and is served under `/<number>/`.

use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use tiny_http::{Header, Request, Response, Server};
use tracing::{debug, warn};

type Roots = Arc<Mutex<Vec<PathBuf>>>;

struct WebServer {
    port: u16,
    roots: Roots,
}

static SERVER: OnceLock<std::result::Result<WebServer, String>> = OnceLock::new();

const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

fn start() -> Result<WebServer> {
    let server = Server::http("127.0.0.1:0").map_err(|e| anyhow!("{e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .context("web server has no ip address")?
        .port();
    let roots: Roots = Arc::default();
    let shared = roots.clone();
    std::thread::Builder::new()
        .name("web-server".into())
        .spawn(move || {
            for request in server.incoming_requests() {
                let roots = shared.clone();
                std::thread::spawn(move || serve(request, &roots));
            }
        })?;
    Ok(WebServer { port, roots })
}

/// The `http://` URL `page` is served at.
pub fn url_for(page: &Path) -> Result<String> {
    let server = SERVER
        .get_or_init(|| start().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| anyhow!("Could not start the web server: {e}"))?;
    let dir = page.parent().context("page has no directory")?;
    let name = page.file_name().context("page has no name")?;
    let mut roots = server.roots.lock().unwrap();
    let n = match roots.iter().position(|r| r == dir) {
        Some(n) => n,
        None => {
            roots.push(dir.to_owned());
            roots.len() - 1
        }
    };
    let name = utf8_percent_encode(&name.to_string_lossy(), SEGMENT).to_string();
    Ok(format!("http://127.0.0.1:{}/{n}/{name}", server.port))
}

/// The file `url` names under one of `roots`, refusing anything that climbs
/// out of it.
fn resolve(roots: &[PathBuf], url: &str) -> Option<PathBuf> {
    let path = url.split(['?', '#']).next()?;
    let path = percent_decode_str(path).decode_utf8().ok()?;
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let root = roots.get(parts.next()?.parse::<usize>().ok()?)?;
    let rel = Path::new(parts.next().unwrap_or(""));
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    let file = root.join(rel);
    if file.is_dir() {
        return Some(file.join("index.html"));
    }
    Some(file)
}

fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" | "glsl" | "frag" | "vert" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn serve(request: Request, roots: &Mutex<Vec<PathBuf>>) {
    let file = resolve(&roots.lock().unwrap(), request.url());
    let result = match file.as_deref().map(|f| (f, File::open(f))) {
        Some((path, Ok(file))) => {
            debug!("web server: {path:?}");
            let header = Header::from_bytes("Content-Type", content_type(path)).unwrap();
            request.respond(Response::from_file(file).with_header(header))
        }
        _ => {
            warn!("web server: no file for {}", request.url());
            request.respond(Response::from_string("Not found").with_status_code(404))
        }
    };
    if let Err(err) = result {
        debug!("web server: {err}");
    }
}

#[cfg(test)]
#[path = "tests/server_tests.rs"]
mod tests;
