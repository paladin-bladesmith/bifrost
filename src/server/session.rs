use anyhow::{Context, Result};
use log::info;
use solana_sdk::transaction::Transaction;
use std::sync::Arc;
use crate::{constants::{DEFAULT_TPU_ADDRESS, MAX_TRANSACTION_SIZE}, tpu_client::TpuConnectionManager};


/// Handles an individual WebTransport session.
///
/// Accepts bidirectional streams, reads transaction data, deserializes it,
/// and forwards to the TPU.
///
/// # Arguments
///
/// * `session` - The WebTransport session
/// * `tpu_manager` - Shared TPU connection manager
///
/// # Errors
///
/// Returns an error if:
/// - Stream acceptance fails
/// - Transaction reading fails
/// - Deserialization fails
/// - TPU forwarding fails
pub async fn handle_session(
    session: web_transport_quinn::Session,
    tpu_manager: Arc<TpuConnectionManager>,
) -> Result<()> {
    info!("Handling session from {}", session.remote_address());

    loop {
        match session.accept_bi().await {
            Ok((mut send, mut recv)) => {
                info!("New stream opened");

                // Read raw transaction data from WebTransport
                let tx_data = recv
                    .read_to_end(MAX_TRANSACTION_SIZE)
                    .await
                    .context("Failed to read transaction")?;

                info!("Received transaction: {} bytes", tx_data.len());

                // Deserialize at the boundary - fail fast if invalid
                let transaction: Transaction = bincode::deserialize(&tx_data)
                    .context("Failed to deserialize transaction")?;

                info!(
                    "Transaction signature: {}, accounts: {}",
                    transaction.signatures.first()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "none".to_string()),
                    transaction.message.account_keys.len()
                );

                // Forward the deserialized transaction to TPU
                match tpu_manager.send_transaction(&tx_data).await {
                    Ok(confirmation) => {
                        info!(
                            "Transaction forwarded successfully (latency: {:?})",
                            confirmation.latency
                        );
                        send.write_all(b"OK").await?;
                    }
                    Err(e) => {
                        log::error!("Failed to forward transaction: {}", e);
                        let error_msg = format!("ERROR: {}", e);
                        send.write_all(error_msg.as_bytes()).await?;
                    }
                }

                send.finish()?;
            }
            Err(e) => {
                log::error!("Failed to accept stream: {}", e);
                break;
            }
        }
    }

    Ok(())
}
