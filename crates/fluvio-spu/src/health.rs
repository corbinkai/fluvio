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
