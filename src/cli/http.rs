//! The HTTPS client shared by the commands that fetch from the network.

use std::time::Duration;

/// An agent that speaks HTTPS through rustls with the pure-Rust crypto
/// provider, giving up on a request after `timeout` in all.
pub fn agent(timeout: Duration) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .unversioned_rustls_crypto_provider(std::sync::Arc::new(rustls_pure::provider()))
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(timeout))
        .build()
        .new_agent()
}

/// Serve one HTTP response on a local port, giving the server's base URL.
#[cfg(test)]
pub(crate) fn serve_once(status: &'static str, body: &'static str) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
            line.clear();
        }
        let mut stream = stream;
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    format!("http://{addr}")
}
