use anyhow::{Context, Result, anyhow};
use dashmap::DashMap;
use log::{debug, info};
use quinn::{
    ClientConfig, Connection as QuinnConnection, Endpoint, IdleTimeout, TransportConfig,
    crypto::rustls::QuicClientConfig,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::u8;
use tokio::sync::RwLock;

use crate::tpu_client::LeaderTracker;

const ALPN_TPU_PROTOCOL_ID: &[u8] = b"solana-tpu";
const QUIC_MAX_TIMEOUT: Duration = Duration::from_secs(5);
const QUIC_KEEP_ALIVE: Duration = Duration::from_secs(4);

/// Result of a transaction delivery attempt.
#[derive(Debug, Clone)]
pub struct DeliveryConfirmation {
    pub delivered: bool,
    pub latency: Duration,
}

#[derive(Default, Debug)]
pub struct Connection {
    conn: Option<QuinnConnection>,
}

/// Manages QUIC connections to Solana TPU endpoints.
///
/// Maintains a connection pool and handles automatic reconnection.
#[derive(Debug)]
pub struct TpuConnectionManager {
    endpoint: Endpoint,
    connections: Arc<RwLock<DashMap<String, Connection>>>,
    leader_tracker: Arc<LeaderTracker>,
}

impl TpuConnectionManager {
    /// Creates a new TPU connection manager.
    ///
    /// # Errors
    ///
    /// Returns an error if the QUIC endpoint cannot be initialized.
    pub fn new(leader_tracker: Arc<LeaderTracker>) -> Result<Self> {
        info!("Creating TPU connection manager");

        let client_certificate = solana_tls_utils::QuicClientCertificate::new(None);

        let mut crypto = solana_tls_utils::tls_client_config_builder()
            .with_client_auth_cert(
                vec![client_certificate.certificate.clone()],
                client_certificate.key.clone_key(),
            )
            .expect("Failed to set QUIC client certificates");

        crypto.enable_early_data = true;
        crypto.alpn_protocols = vec![ALPN_TPU_PROTOCOL_ID.to_vec()];

        let transport_config = {
            let mut res = TransportConfig::default();
            let timeout = IdleTimeout::try_from(QUIC_MAX_TIMEOUT).unwrap();
            res.max_idle_timeout(Some(timeout));
            res.keep_alive_interval(Some(QUIC_KEEP_ALIVE));
            res.send_fairness(false);
            res
        };

        let mut config = ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto).unwrap()));
        config.transport_config(Arc::new(transport_config));

        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(config);

        info!("TPU connection manager created");

        Ok(Self {
            endpoint,
            connections: Arc::new(RwLock::new(DashMap::new())),
            leader_tracker,
        })
    }

    /// Sends a Solana transaction to the specified validator's TPU.
    ///
    /// # Arguments
    ///
    /// * `validator` - TPU address (e.g., "127.0.0.1:8001")
    /// * `transaction` - The transaction to send
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Serialization fails
    /// - Connection fails
    /// - Stream creation fails
    /// - Data transmission fails
    pub async fn send_transaction(&self, tx_data: &[u8]) -> Result<DeliveryConfirmation> {
        debug!("Packet preview: {:02x?}", &tx_data[..tx_data.len().min(32)]);

        let start = Instant::now();
        let leaders = self.leader_tracker.get_leaders().await;
        let mut tx_sent = false;
        println!("leaders: {:#?}", leaders);

        for (leader_identity, leader_socket, curr_slot) in leaders {
            info!("Slot: {}", curr_slot);
            if let Ok(Some(conn)) = self.get_connection(&leader_socket).await {
                info!(
                    "Sending {} bytes to {} at: {}",
                    tx_data.len(),
                    leader_identity,
                    leader_socket
                );

                let mut send_stream = conn.open_uni().await.context("Failed to open uni stream")?;

                send_stream
                    .write_all(&tx_data)
                    .await
                    .context("Failed to write transaction data")?;

                send_stream.finish().context("Failed to finish stream")?;

                tx_sent = true;
            } else {
                info!(
                    "Connection failed for {} at: {}",
                    leader_identity, leader_socket
                );
            };
        }

        if !tx_sent {
            return Err(anyhow!("Failed sending TX"));
        }

        Ok(DeliveryConfirmation {
            delivered: true,
            latency: start.elapsed(),
        })
    }

    pub async fn get_connection(&self, validator: &str) -> Result<Option<QuinnConnection>> {
        let conns = self.connections.read().await;

        if let Some(conn) = conns.get(validator) {
            // If we are already connected check connection is active
            match &conn.conn {
                Some(conn) => {
                    if conn.close_reason().is_none() {
                        debug!("Reusing connection to {}", validator);
                        return Ok(Some(conn.clone()));
                    }
                }
                None => return Err(anyhow!("No connection is open")),
            }
        }

        return Ok(None);
    }

    /// Gets an existing connection or creates a new one to the validator.
    pub async fn get_or_create_connection(&self, validator: &str) -> Result<QuinnConnection> {
        match self.get_connection(validator).await {
            // we have active connection
            Ok(Some(conn)) => return Ok(conn),
            // We have no connection, try to connect
            Ok(None) => (),
            // We are are still trying to connect
            Err(_) => return Err(anyhow!("Already connecting")),
        }

        let conns = self.connections.write().await;
        if let Some(conn) = conns.get(validator)
            && let None = conn.conn
        {
            return Err(anyhow!("Already connecting"));
        }
        conns.insert(validator.to_string(), Connection::default());
        drop(conns);

        debug!("Creating new connection to {}", validator);
        let addr: SocketAddr = validator.parse().context("Invalid validator address")?;

        let connection = match self.endpoint.connect(addr, "solana")?.into_0rtt() {
            Ok((conn, rtt_accepted)) => {
                debug!("Waiting for 0-RTT for: {}", addr);

                if rtt_accepted.await {
                    debug!("0-RTT accepted");
                }
                conn
            }
            Err(connecting) => {
                debug!("0-RTT not accepted, waiting for handshake to complete");
                match connecting.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        // Failed to connect, return error and remove from list of connections
                        self.connections.write().await.remove(validator);
                        return Err(e.into());
                    }
                }
            }
        };

        self.connections.write().await.insert(
            validator.to_string(),
            Connection {
                conn: Some(connection.clone()),
            },
        );
        info!("Connected to {}", validator);

        Ok(connection)
    }

    /// Returns the number of active connections.
    pub async fn connection_count(&self) -> usize {
        self.connections.read().await.len()
    }

    /// Closes all connections.
    pub async fn close_all(&self) {
        let connections = self.connections.write().await;
        for conn in connections.iter() {
            if let Some(conn) = &conn.value().conn {
                conn.close(0u32.into(), b"shutdown");
            }
        }
        connections.clear();
    }
}

impl Drop for TpuConnectionManager {
    fn drop(&mut self) {
        let handle = tokio::runtime::Handle::current();

        handle.block_on(self.close_all());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager_creation() {
        let leader_tracker = Arc::new(LeaderTracker::default());
        let manager = TpuConnectionManager::new(leader_tracker);
        assert!(manager.is_ok());
    }

    #[tokio::test]
    async fn test_connection_count() {
        let leader_tracker = Arc::new(LeaderTracker::default());
        let manager = TpuConnectionManager::new(leader_tracker).unwrap();
        assert_eq!(manager.connection_count().await, 0);
    }
}
