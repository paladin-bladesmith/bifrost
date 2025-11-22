use anyhow::Context;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use solana_system_interface::instruction as system_instruction;
use std::{fs, io, thread::sleep, time::Duration};
use url::Url;

const BIFROST_URL: &str = "https://127.0.0.1:4433";
const CERT_PATH: &str = "certs/cert.pem";
const RPC_URL: &str = "https://api.devnet.solana.com";

const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    println!("Connecting to Bifrost at {}", BIFROST_URL);

    let session = connect_to_bifrost().await?;
    println!("Connected to Bifrost successfully");

    let mut success = vec![];
    for _ in 0..10 {
        let transaction = create_test_without_airdrop().await?;
        println!(
            "Created transaction with signature: {}",
            transaction.signatures[0]
        );

        match send_transaction(&session, transaction.clone()).await {
            Ok(_) => success.push(true),
            Err(_) => (),
        };
        sleep(Duration::from_millis(1200));
    }

    println!(
        "Successfully sent {} out of 100 transactions",
        success.len()
    );

    Ok(())
}

async fn create_test_without_airdrop() -> anyhow::Result<Transaction> {
    let rpc_client = RpcClient::new(RPC_URL.to_string());

    let payer = Keypair::from_base58_string(
        "",
    );

    println!("Payer {}", payer.pubkey());

    build_transfer_transaction(&rpc_client, &payer)
}

/// Connects to Bifrost WebTransport server.
async fn connect_to_bifrost() -> anyhow::Result<web_transport_quinn::Session> {
    let cert_file = fs::File::open(CERT_PATH)
        .context(format!("Failed to open certificate file: {}", CERT_PATH))?;

    let mut cert_reader = io::BufReader::new(cert_file);
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<_, _>>()
        .context("Failed to parse certificates")?;

    anyhow::ensure!(!certs.is_empty(), "No certificates found in {}", CERT_PATH);

    let client = web_transport_quinn::ClientBuilder::new()
        .with_server_certificates(certs)
        .context("Failed to create WebTransport client")?;

    let url = Url::parse(BIFROST_URL).context(format!("Invalid Bifrost URL: {}", BIFROST_URL))?;

    client
        .connect(url)
        .await
        .context("Failed to connect to Bifrost")
}

/// Creates a test transaction by requesting an airdrop and transferring funds.
async fn _create_test_transaction() -> anyhow::Result<Transaction> {
    let rpc_client = RpcClient::new(RPC_URL.to_string());

    let payer = Keypair::new();
    println!("Generated keypair: {}", payer.pubkey());

    // _request_and_confirm_airdrop(&rpc_client, &payer).await?;

    let balance = rpc_client.get_balance(&payer.pubkey())?;
    println!(
        "Payer balance: {} SOL",
        balance as f64 / LAMPORTS_PER_SOL as f64
    );

    let transaction = build_transfer_transaction(&rpc_client, &payer)?;
    println!("Transaction created successfully");

    Ok(transaction)
}

/// Requests an airdrop and waits for confirmation.
async fn _request_and_confirm_airdrop(client: &RpcClient, keypair: &Keypair) -> anyhow::Result<()> {
    let airdrop_amount = 1 * LAMPORTS_PER_SOL;

    println!(
        "Requesting airdrop of {} SOL",
        airdrop_amount / LAMPORTS_PER_SOL
    );
    let signature = client
        .request_airdrop(&keypair.pubkey(), airdrop_amount)
        .context("Failed to request airdrop")?;

    println!("Airdrop requested, waiting for confirmation...");

    loop {
        if client.confirm_transaction(&signature)? {
            println!("Airdrop confirmed!");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Builds a transfer transaction.
fn build_transfer_transaction(client: &RpcClient, payer: &Keypair) -> anyhow::Result<Transaction> {
    // get rent exempt minimum from rpc, or just oversend lol
    let rent_exempt = client
        .get_minimum_balance_for_rent_exemption(0)
        .context("Failed to get rent exemption amount")?;

    let recipient = Pubkey::new_unique();
    let transfer_amount = rent_exempt + 100_000; // Rent-exempt + 100k lamports

    println!("Transferring {} lamports to {}", transfer_amount, recipient);
    println!("Rent-exempt minimum: {} lamports", rent_exempt);

    let recent_blockhash = client
        .get_latest_blockhash()
        .context("Failed to get recent blockhash")?;

    let instruction = system_instruction::transfer(&payer.pubkey(), &recipient, transfer_amount);

    let mut transaction = Transaction::new_with_payer(&[instruction], Some(&payer.pubkey()));

    transaction.sign(&[payer], recent_blockhash);

    Ok(transaction)
}

/// Sends a transaction to Bifrost via WebTransport.
async fn send_transaction(
    session: &web_transport_quinn::Session,
    transaction: Transaction,
) -> anyhow::Result<()> {
    let tx_bytes = bincode::serialize(&transaction).context("Failed to serialize transaction")?;

    println!("Sending transaction ({} bytes)", tx_bytes.len());

    let (mut send, mut recv) = session
        .open_bi()
        .await
        .context("Failed to open bidirectional stream")?;

    send.write_all(&tx_bytes)
        .await
        .context("Failed to write transaction data")?;

    send.finish().context("Failed to finish send stream")?;

    let response = recv
        .read_to_end(1024)
        .await
        .context("Failed to read response")?;

    let response_str = String::from_utf8_lossy(&response);
    println!("Server response: {}", response_str);

    if !response_str.starts_with("OK") {
        anyhow::bail!("Transaction rejected: {}", response_str);
    }

    Ok(())
}
