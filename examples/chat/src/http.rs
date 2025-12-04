//! Simple HTTP server for serving the web client.
//!
//! This is a minimal HTTP/1.1 server that serves the static HTML file.
//! For production use, consider using a proper web framework.

use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The HTML content to serve (embedded at compile time).
const INDEX_HTML: &str = include_str!("../static/index.html");

/// Start the HTTP server.
pub async fn serve(addr: SocketAddr) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;

    loop {
        let (mut socket, peer_addr) = listener.accept().await?;

        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];

            // Read the request
            match socket.read(&mut buf).await {
                Ok(0) => (),
                Ok(n) => {
                    let request = String::from_utf8_lossy(&buf[..n]);

                    // Parse the request line
                    let first_line = request.lines().next().unwrap_or("");
                    let parts: Vec<&str> = first_line.split_whitespace().collect();

                    if parts.len() >= 2 {
                        let method = parts[0];
                        let path = parts[1];

                        tracing::debug!(%peer_addr, %method, %path, "HTTP request");

                        // Handle the request
                        let response = match (method, path) {
                            ("GET", "/" | "/index.html") => {
                                format!(
                                    "HTTP/1.1 200 OK\r\n\
                                     Content-Type: text/html; charset=utf-8\r\n\
                                     Content-Length: {}\r\n\
                                     Connection: close\r\n\
                                     \r\n\
                                     {}",
                                    INDEX_HTML.len(),
                                    INDEX_HTML
                                )
                            }
                            ("GET", "/favicon.ico") => "HTTP/1.1 204 No Content\r\n\
                                 Connection: close\r\n\
                                 \r\n"
                                .to_string(),
                            _ => {
                                let body = "404 Not Found";
                                format!(
                                    "HTTP/1.1 404 Not Found\r\n\
                                     Content-Type: text/plain\r\n\
                                     Content-Length: {}\r\n\
                                     Connection: close\r\n\
                                     \r\n\
                                     {}",
                                    body.len(),
                                    body
                                )
                            }
                        };

                        // Send the response
                        if let Err(e) = socket.write_all(response.as_bytes()).await {
                            tracing::warn!(%peer_addr, error = %e, "Failed to send response");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(%peer_addr, error = %e, "Failed to read request");
                }
            }
        });
    }
}
