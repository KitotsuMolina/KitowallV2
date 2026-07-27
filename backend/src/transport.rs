use std::fmt;
use std::thread;
use std::time::Duration;

use ureq::Agent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

pub trait HttpTransport {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError>;

    fn get_with_headers(
        &self,
        url: &str,
        _headers: &[(&str, &str)],
    ) -> Result<HttpResponse, TransportError> {
        self.get(url)
    }
}

#[derive(Debug, Clone)]
pub struct UreqTransportConfig {
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub body_timeout: Duration,
    pub retries: u32,
    pub retry_backoff: Duration,
    pub max_response_bytes: usize,
    pub user_agent: String,
}

impl Default for UreqTransportConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(5),
            body_timeout: Duration::from_secs(20),
            retries: 2,
            retry_backoff: Duration::from_millis(300),
            max_response_bytes: 100 * 1024 * 1024,
            user_agent: format!("kitowall/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UreqTransport {
    agent: Agent,
    config: UreqTransportConfig,
}

impl UreqTransport {
    pub fn new(config: UreqTransportConfig) -> Self {
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(config.timeout))
            .timeout_connect(Some(config.connect_timeout))
            .timeout_recv_body(Some(config.body_timeout))
            .max_redirects(5)
            .user_agent(&config.user_agent)
            .build()
            .into();
        Self { agent, config }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new(UreqTransportConfig::default())
    }
}

impl HttpTransport for UreqTransport {
    fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        self.get_with_headers(url, &[])
    }

    fn get_with_headers(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<HttpResponse, TransportError> {
        if !matches!(url.split(':').next(), Some("http" | "https")) {
            return Err(TransportError::new(
                "only http:// and https:// URLs are allowed",
            ));
        }

        let attempts = self.config.retries.saturating_add(1);
        let mut last_error = None;
        for attempt in 1..=attempts {
            match self.request_once(url, headers) {
                Ok(response) if retryable_status(response.status) && attempt < attempts => {
                    last_error = Some(TransportError::new(format!(
                        "transient HTTP status {} on attempt {attempt}",
                        response.status
                    )));
                }
                Ok(response) => return Ok(response),
                Err(error) if attempt < attempts => last_error = Some(error),
                Err(error) => return Err(error),
            }
            thread::sleep(self.config.retry_backoff.saturating_mul(attempt));
        }
        Err(last_error.unwrap_or_else(|| TransportError::new("HTTP request failed")))
    }
}

impl UreqTransport {
    fn request_once(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<HttpResponse, TransportError> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request
            .call()
            .map_err(|error| TransportError::new(error.to_string()))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response
            .body_mut()
            .with_config()
            .limit(self.config.max_response_bytes.saturating_add(1) as u64)
            .read_to_vec()
            .map_err(|error| TransportError::new(format!("response body error: {error}")))?;
        if body.len() > self.config.max_response_bytes {
            return Err(TransportError::new(format!(
                "response exceeds {} byte limit",
                self.config.max_response_bytes
            )));
        }
        Ok(HttpResponse {
            status,
            body,
            content_type,
        })
    }
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    pub message: String,
}

impl TransportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn production_transport_retries_transient_status_on_local_server() {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind local test server: {error}"),
        };
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(0_u32));
        let server_requests = Arc::clone(&requests);
        let server = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                let _ = stream.read(&mut request).unwrap();
                *server_requests.lock().unwrap() += 1;
                let response = if index == 0 {
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
                } else {
                    let body = "image";
                    format!("HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                };
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let transport = UreqTransport::new(UreqTransportConfig {
            retry_backoff: Duration::ZERO,
            timeout: Duration::from_secs(2),
            ..UreqTransportConfig::default()
        });
        let response = transport
            .get(&format!("http://{address}/wall.jpg"))
            .unwrap();
        server.join().unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"image");
        assert_eq!(*requests.lock().unwrap(), 2);
    }

    #[test]
    fn production_transport_rejects_non_http_schemes_without_io() {
        let error = UreqTransport::default()
            .get("file:///etc/passwd")
            .unwrap_err();
        assert!(error.to_string().contains("only http"));
    }
}
