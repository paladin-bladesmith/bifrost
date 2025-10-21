#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("Invalid transaction: {0}")]
    InvalidTransaction(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Delivery timeout")]
    DeliveryTimeout,
    // ... more variants
}
