use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::Mutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, error};

use fluvio_future::task::spawn;
use fluvio_types::event::StickyEvent;

pub const SPU_HEALTH_PORT: u16 = 9008;

pub struct HealthState {
    sc_connected: AtomicBool,
    storage_ready: AtomicBool,
    shutdown_events: Mutex<Vec<Arc<StickyEvent>>>,
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            sc_connected: AtomicBool::new(false),
            storage_ready: AtomicBool::new(false),
            shutdown_events: Mutex::new(Vec::new()),
        }
    }

    pub fn set_sc_connected(&self, connected: bool) {
        self.sc_connected.store(connected, Ordering::Relaxed);
    }

    pub fn set_storage_ready(&self, ready: bool) {
        self.storage_ready.store(ready, Ordering::Relaxed);
    }

    pub fn is_ready(&self) -> bool {
        self.sc_connected.load(Ordering::Relaxed)
            && self.storage_ready.load(Ordering::Relaxed)
    }

    /// Register a server shutdown event to be notified on graceful shutdown.
    pub fn register_shutdown_event(&self, event: Arc<StickyEvent>) {
        self.shutdown_events.lock().unwrap().push(event);
    }

    /// Notify all registered shutdown events to drain connections.
    pub fn trigger_shutdown(&self) {
        let events = self.shutdown_events.lock().unwrap();
        for event in events.iter() {
            event.notify();
        }
    }
}

const HTTP_200: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const HTTP_503: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 9\r\n\r\nnot ready";

pub fn start_health_server(health: Arc<HealthState>) {
    spawn(async move {
        let port = std::env::var("SPU_HEALTH_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(SPU_HEALTH_PORT);

        let addr = format!("0.0.0.0:{port}");
        let listener = match TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(err) => {
                error!(%addr, %err, "failed to bind health server");
                return;
            }
        };
        info!(%addr, "health server started");

        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(err) => {
                    error!(%err, "health server accept error");
                    continue;
                }
            };

            let health = health.clone();
            spawn(async move {
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf).await;
                let request = String::from_utf8_lossy(&buf);

                let response = if request.starts_with("GET /readyz") {
                    if health.is_ready() {
                        HTTP_200
                    } else {
                        HTTP_503
                    }
                } else {
                    // /healthz and anything else returns 200 (process is alive)
                    HTTP_200
                };

                let _ = stream.write_all(response).await;
                let _ = stream.shutdown().await;
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[test]
    fn test_health_state_defaults_not_ready() {
        let state = HealthState::new();
        assert!(!state.is_ready());
    }

    #[test]
    fn test_health_state_ready_when_both_set() {
        let state = HealthState::new();
        state.set_sc_connected(true);
        state.set_storage_ready(true);
        assert!(state.is_ready());
    }

    #[test]
    fn test_health_state_not_ready_sc_disconnected() {
        let state = HealthState::new();
        state.set_sc_connected(false);
        state.set_storage_ready(true);
        assert!(!state.is_ready());
    }

    #[test]
    fn test_health_state_not_ready_storage_not_ready() {
        let state = HealthState::new();
        state.set_sc_connected(true);
        state.set_storage_ready(false);
        assert!(!state.is_ready());
    }

    #[test]
    fn test_shutdown_event_registration() {
        let state = HealthState::new();
        let event1 = StickyEvent::shared();
        let event2 = StickyEvent::shared();
        state.register_shutdown_event(event1);
        state.register_shutdown_event(event2);
        assert_eq!(state.shutdown_events.lock().unwrap().len(), 2);
    }

    #[test]
    fn test_trigger_shutdown_notifies_all_events() {
        let state = HealthState::new();
        let event1 = StickyEvent::shared();
        let event2 = StickyEvent::shared();
        state.register_shutdown_event(event1.clone());
        state.register_shutdown_event(event2.clone());
        state.trigger_shutdown();
        assert!(event1.is_set());
        assert!(event2.is_set());
    }

    #[tokio::test]
    async fn test_healthz_returns_200() {
        let health = Arc::new(HealthState::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let health_clone = health.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf).await;
            let request = String::from_utf8_lossy(&buf);
            let response = if request.starts_with("GET /readyz") {
                if health_clone.is_ready() { HTTP_200 } else { HTTP_503 }
            } else {
                HTTP_200
            };
            let _ = stream.write_all(response).await;
            let _ = stream.shutdown().await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"GET /healthz HTTP/1.1\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 256];
        let n = stream.read(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
    }

    #[tokio::test]
    async fn test_readyz_returns_503_when_not_ready() {
        let health = Arc::new(HealthState::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let health_clone = health.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf).await;
            let request = String::from_utf8_lossy(&buf);
            let response = if request.starts_with("GET /readyz") {
                if health_clone.is_ready() { HTTP_200 } else { HTTP_503 }
            } else {
                HTTP_200
            };
            let _ = stream.write_all(response).await;
            let _ = stream.shutdown().await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"GET /readyz HTTP/1.1\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 256];
        let n = stream.read(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 503"));
    }

    #[tokio::test]
    async fn test_readyz_returns_200_when_ready() {
        let health = Arc::new(HealthState::new());
        health.set_sc_connected(true);
        health.set_storage_ready(true);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let health_clone = health.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf).await;
            let request = String::from_utf8_lossy(&buf);
            let response = if request.starts_with("GET /readyz") {
                if health_clone.is_ready() { HTTP_200 } else { HTTP_503 }
            } else {
                HTTP_200
            };
            let _ = stream.write_all(response).await;
            let _ = stream.shutdown().await;
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"GET /readyz HTTP/1.1\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 256];
        let n = stream.read(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
    }
}
