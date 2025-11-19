use anyhow::Context;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use solana_system_interface::instruction as system_instruction;
use std::{fs, io, thread::sleep, time::Duration};
use url::Url;

const BIFROST_URL: &str = "https://127.0.0.1:4433";
const CERT_PATH: &str = "certs/cert.pem";
const RPC_URL: &str = "https://api.testnet.solana.com"; // For Airdrop
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
const TEST_KEYPAIR1: [u8; 32] = [
    143, 239, 226, 151, 40, 210, 184, 162, 99, 53, 65, 216, 6, 120, 57, 199, 209, 74, 234, 182, 12,
    47, 217, 197, 97, 253, 227, 181, 80, 28, 231, 32,
];
const TEST_KEYPAIR2: [u8; 32] = [
    220, 94, 77, 130, 91, 80, 102, 186, 9, 108, 173, 197, 10, 39, 74, 254, 32, 141, 209, 212, 46,
    109, 223, 122, 172, 67, 237, 37, 38, 102, 26, 7,
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    println!("Connecting to Bifrost at {}", BIFROST_URL);

    let session = connect_to_bifrost().await?;
    println!("Connected to Bifrost successfully");

    let transaction = create_test_transaction().await?;
    println!(
        "Created transaction with signature: {}",
        transaction.signatures[0]
    );

    let mut success = vec![];
    for _ in 0..100 {
        match send_transaction(&session, transaction.clone()).await {
            Ok(_) => success.push(true),
            Err(_) => (),
        };
        sleep(Duration::from_secs(1));
    }

    println!(
        "Successfully sent {} out of 100 transactions",
        success.len()
    );

    Ok(())
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
async fn create_test_transaction() -> anyhow::Result<Transaction> {
    let rpc_client = RpcClient::new(RPC_URL.to_string());

    let payer = Keypair::new_from_array(TEST_KEYPAIR1);
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
/// NOTE: For some reason this was failing, in my opinion a better way would be to create 2 test pubkeys
/// request airdrop on website to one of them, and switch bettwen them when it gets empty.
async fn _request_and_confirm_airdrop(client: &RpcClient, keypair: &Keypair) -> anyhow::Result<()> {
    let airdrop_amount = 2 * LAMPORTS_PER_SOL;

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

    let recipient = Keypair::new_from_array(TEST_KEYPAIR2).pubkey();
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

    // NOTE: Transaction might have been sent, but doesn't mean it was able to land.
    // To fully confirm, you need to check on chain
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
