use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Spawn a TCP server that serves exactly one HTTP response and then shuts down.
/// Returns the base URL (e.g. `http://127.0.0.1:PORT`) or `None` if binding fails.
pub fn spawn_one_shot_http_server(status_line: &str, body: &str) -> Option<String> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let addr = listener.local_addr().unwrap();
    let status = status_line.to_string();
    let response_body = body.to_string();

    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut req_buf = [0u8; 1024];
            let _ = stream.read(&mut req_buf);
            let response = format!(
                "{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                response_body.len(),
                response_body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    Some(format!("http://{}", addr))
}

/// Spawn a TCP server that serves a sequence of HTTP responses in order,
/// capturing each request as a `String`. Returns `(base_url, captured_requests)`.
pub fn spawn_sequence_http_server(
    responses: &[(&str, &str)],
) -> Option<(String, Arc<Mutex<Vec<String>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let addr = listener.local_addr().unwrap();
    let planned_responses: Vec<(String, String)> = responses
        .iter()
        .map(|(status, body)| ((*status).to_string(), (*body).to_string()))
        .collect();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured_requests = Arc::clone(&requests);

    std::thread::spawn(move || {
        for (status, response_body) in planned_responses {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut req_buf = [0u8; 4096];
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let size = stream.read(&mut req_buf).unwrap_or(0);
            captured_requests
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&req_buf[..size]).to_string());
            let response = format!(
                "{}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                status,
                response_body.len(),
                response_body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    Some((format!("http://{}", addr), requests))
}
