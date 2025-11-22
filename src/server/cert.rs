//! TLS certificate loading utilities.

use anyhow::{Context, Result};
use log::info;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile::{certs, private_key};
use std::fs::File;
use std::io::BufReader;

/// Loads TLS certificates and private key from PEM files.
///
/// # Arguments
///
/// * `cert_path` - Path to certificate file
/// * `key_path` - Path to private key file
///
/// # Returns
///
/// A tuple containing the certificate chain and private key
///
/// # Errors
///
/// Returns an error if:
/// - File reading fails
/// - Certificate parsing fails
/// - No certificates found
/// - Private key parsing fails
pub fn load_certificates(
    cert_path: &str,
    key_path: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_file =
        File::open(cert_path).context(format!("Failed to open certificate file: {}", cert_path))?;
    let mut cert_reader = BufReader::new(cert_file);

    let cert_chain: Vec<CertificateDer> = certs(&mut cert_reader)
        .collect::<Result<_, _>>()
        .context("Failed to parse certificates")?;

    if cert_chain.is_empty() {
        anyhow::bail!("No certificates found in {}", cert_path);
    }

    let key_file =
        File::open(key_path).context(format!("Failed to open private key file: {}", key_path))?;
    let mut key_reader = BufReader::new(key_file);

    let private_key = private_key(&mut key_reader)
        .context("Failed to parse private key")?
        .context("No private key found")?;

    info!("Loaded {} certificate(s) and private key", cert_chain.len());

    Ok((cert_chain, private_key))
}
