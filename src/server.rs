//! Локальный HTTP-сервер: через него нейросети и скрипты управляют программой (строят детали).
//! Только стандартная библиотека. Слушает исключительно 127.0.0.1, а запросы из браузера
//! (с заголовком Origin) и с чужим именем хоста отклоняются — так сайт в браузере не сможет
//! управлять программой.
//!
//! Маршруты: `GET /` и `GET /help` — инструкция (docs/API.md), `GET /health` — проверка,
//! `POST /api` — команда или пачка команд в JSON (заголовок Content-Type: application/json).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Запрос к программе: тело JSON и канал, в который интерфейс пришлёт ответ.
pub struct ApiRequest {
    pub body: String,
    pub reply: mpsc::Sender<String>,
}

/// Работающий сервер; при уничтожении перестаёт принимать подключения.
pub struct ApiServer {
    stop: Arc<AtomicBool>,
    pub port: u16,
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

const HELP: &str = include_str!("../docs/API.md");
const MAX_BODY: usize = 16 * 1024 * 1024;

/// Запускает сервер на `port`. Запросы приходят в возвращённый канал и разбираются в потоке интерфейса
/// (ядро OpenCascade не потокобезопасно); `ctx` будит интерфейс, когда пришёл запрос.
pub fn start(port: u16, ctx: eframe::egui::Context) -> Result<(ApiServer, mpsc::Receiver<ApiRequest>), String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("порт {port} занят или недоступен: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    // Ошибка одного соединения не должна останавливать сервер.
                    let _ = handle(stream, port, &tx, &ctx);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(30)),
            }
        }
    });
    Ok((ApiServer { stop, port }, rx))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

fn json_error(stream: &mut TcpStream, status: &str, message: &str) -> std::io::Result<()> {
    let body = serde_json::json!({ "ok": false, "error": message }).to_string();
    respond(stream, status, "application/json; charset=utf-8", &body)
}

fn handle(
    mut stream: TcpStream,
    port: u16,
    tx: &mpsc::Sender<ApiRequest>,
    ctx: &eframe::egui::Context,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            break p;
        }
        if buf.len() > 64 * 1024 {
            return json_error(&mut stream, "431 Request Header Fields Too Large", "слишком длинные заголовки");
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());

    // Защита от управления из браузера: чужой Origin и подмена хоста (DNS rebinding) не допускаются.
    let host_ok = header("host").is_some_and(|h| {
        let h = h.to_ascii_lowercase();
        [format!("127.0.0.1:{port}"), format!("localhost:{port}"), "127.0.0.1".into(), "localhost".into()].contains(&h)
    });
    if header("origin").is_some() || !host_ok {
        return json_error(&mut stream, "403 Forbidden", "запросы из браузера и с чужим хостом не принимаются");
    }

    match (method, path.split('?').next().unwrap_or("")) {
        ("GET", "/" | "/help") => respond(&mut stream, "200 OK", "text/markdown; charset=utf-8", HELP),
        ("GET", "/health") => respond(&mut stream, "200 OK", "application/json", r#"{"ok":true,"app":"MySolid3D"}"#),
        ("POST", "/api") => {
            if !header("content-type").is_some_and(|t| t.to_ascii_lowercase().contains("application/json")) {
                return json_error(&mut stream, "415 Unsupported Media Type", "нужен заголовок Content-Type: application/json");
            }
            let length: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
            if length > MAX_BODY {
                return json_error(&mut stream, "413 Payload Too Large", "слишком большой запрос (максимум 16 МБ)");
            }
            let mut body: Vec<u8> = buf[header_end + 4..].to_vec();
            while body.len() < length {
                let n = stream.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            body.truncate(length);
            let (reply_tx, reply_rx) = mpsc::channel();
            let request = ApiRequest { body: String::from_utf8_lossy(&body).into_owned(), reply: reply_tx };
            if tx.send(request).is_err() {
                return json_error(&mut stream, "503 Service Unavailable", "программа закрывается");
            }
            ctx.request_repaint();
            match reply_rx.recv_timeout(Duration::from_secs(600)) {
                Ok(answer) => respond(&mut stream, "200 OK", "application/json; charset=utf-8", &answer),
                Err(_) => json_error(&mut stream, "504 Gateway Timeout", "программа не ответила вовремя"),
            }
        }
        ("OPTIONS", _) => json_error(&mut stream, "403 Forbidden", "CORS не поддерживается"),
        _ => json_error(&mut stream, "404 Not Found", "есть только GET /, GET /health и POST /api"),
    }
}
