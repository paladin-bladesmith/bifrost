//! WebTransport server implementation for Bifrost.

mod cert;
mod session;

pub use cert::load_certificates;
pub use session::handle_session;

use crate::tpu_client::{LeaderTracker, TpuConnectionManager};
use anyhow::{Context, Result};
use log::{debug, error, info};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// WebTransport server that accepts connections and forwards transactions to TPU.
pub struct BifrostServer {
    addr: SocketAddr,
    cert_path: String,
    key_path: String,
}

impl BifrostServer {
    /// Creates a new Bifrost server instance.
    ///
    /// # Arguments
    ///
    /// * `addr` - Socket address to bind the server
    /// * `cert_path` - Path to TLS certificate file
    /// * `key_path` - Path to TLS private key file
    pub fn new(addr: SocketAddr, cert_path: &str, key_path: &str) -> Self {
        Self {
            addr,
            cert_path: cert_path.to_string(),
            key_path: key_path.to_string(),
        }
    }

    /// Starts the WebTransport server and begins accepting connections.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Certificate loading fails
    /// - TPU manager initialization fails
    /// - Server binding fails
    pub async fn run(self) -> Result<()> {
        info!("Starting Bifrost on {}", self.addr);

        let (cert_chain, private_key) = load_certificates(&self.cert_path, &self.key_path)
            .context("Failed to load certificates")?;

        // Initialize the LeaderTracker - NOW RETURNS RESULT
        let leader_tracker = Arc::new(
            LeaderTracker::new()
                .await
                .context("Failed to initialize LeaderTracker")?,
        );

        // Spawn the slot_updates listener as a background task
        let leader_tracker_clone = leader_tracker.clone();
        tokio::spawn(async move {
            if let Err(e) = LeaderTracker::run(leader_tracker_clone).await {
                error!("Slot updates listener failed: {}", e);
                // TODO: implement reconnection logic here
            }
        });

        // Spawn task to update leader sockets list every minute
        let leader_tracker_clone = leader_tracker.clone();
        tokio::spawn(async move {
            loop {
                match LeaderTracker::update_leader_sockets(leader_tracker_clone.clone()).await {
                    Ok(_) => debug!("Leader sockets updated successfully"),
                    Err(e) => error!("Failed to update leader sockets: {}", e),
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });

        let tpu_manager = Arc::new(
            TpuConnectionManager::new(leader_tracker.clone())
                .context("Failed to create TPU manager")?,
        );

        // Spawn task to proactively connect to future leaders
        let manager_clone = tpu_manager.clone();
        let leader_tracker_clone = leader_tracker.clone();
        tokio::spawn(async move {
            loop {
                debug!("Pre-connecting to future leaders");
                let leaders = leader_tracker_clone.get_future_leaders(0, 10 * 4).await;

                for (leader_identity, leader_socket, _) in leaders {
                    let mc = manager_clone.clone();
                    tokio::spawn(async move {
                        match mc.get_or_create_connection(&leader_socket).await {
                            Ok(_) => debug!(
                                "Pre-connected to leader {} at {}",
                                leader_identity, leader_socket
                            ),
                            Err(e) => debug!("Failed to pre-connect to {}: {}", leader_socket, e),
                        }
                    });
                }

                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        let mut server = web_transport_quinn::ServerBuilder::new()
            .with_addr(self.addr)
            .with_certificate(cert_chain, private_key)?;

        info!("Listening for WebTransport connections on {}", self.addr);

        // Accept and handle incoming connections
        while let Some(request) = server.accept().await {
            info!("Received connection request: {}", request.url());

            let tpu = tpu_manager.clone();
            tokio::spawn(async move {
                match request.ok().await {
                    Ok(session) => {
                        info!("Session accepted from {}", session.remote_address());
                        if let Err(e) = handle_session(session, tpu).await {
                            error!("Session error: {}", e);
                        }
                    }
                    Err(e) => {
                        error!("Failed to accept session: {}", e);
                    }
                }
            });
        }

        info!("Server shutting down");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_creation() {
        let addr = "127.0.0.1:4433".parse().unwrap();
        let server = BifrostServer::new(addr, "certs/cert.pem", "certs/key.pem");
        assert_eq!(server.addr.port(), 4433);
    }

    #[tokio::test]
    async fn test_tpu_client_creation() {
        use crate::tpu_client::TpuConnectionManager;

        // LeaderTracker::new() now returns Result
        let leader_tracker = Arc::new(
            LeaderTracker::new()
                .await
                .expect("Failed to initialize LeaderTracker"),
        );

        let result = TpuConnectionManager::new(leader_tracker);
        match result {
            Ok(_) => println!("TPU client created successfully"),
            Err(e) => panic!("TPU client failed: {}", e),
        }
    }
}
