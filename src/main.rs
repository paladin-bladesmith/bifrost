use anyhow::Result;
use bifrost::server::BifrostServer;

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    
    env_logger::init();

    let addr = "[::]:4433".parse()?;
    let server = BifrostServer::new(addr, "certs/cert.pem", "certs/key.pem");

    server.run().await?;

    Ok(())
}