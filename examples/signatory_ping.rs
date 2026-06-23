//! Connect to cdk-signatory over gRPC (optionally bootstrap the first sat keyset).
use cdk_common::nuts::nut02::KeySetVersion;
use cdk_common::CurrencyUnit;
use cdk_signatory::signatory::{RotateKeyArguments, Signatory};
use cdk_signatory::SignatoryRpcClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("URL").unwrap_or_else(|_| "https://localhost:3340".into());
    let tls = std::env::var("TLS_DIR").ok();
    let client = SignatoryRpcClient::new(url.clone(), tls.as_deref()).await?;
    println!("connected: {}", client.name());

    let mut ks = client.keysets().await?;
    if ks.keysets.is_empty() && std::env::var("BOOTSTRAP").ok().as_deref() == Some("1") {
        let amounts: Vec<u64> = (0..32).map(|i| 2u64.pow(i)).collect();
        let created = client
            .rotate_keyset(RotateKeyArguments {
                unit: CurrencyUnit::Sat,
                amounts,
                input_fee_ppk: 0,
                keyset_id_type: KeySetVersion::Version00,
                final_expiry: None,
            })
            .await?;
        println!("bootstrapped keyset id={}", created.id);
        ks = client.keysets().await?;
    }

    println!("pubkey={}", ks.pubkey);
    for entry in &ks.keysets {
        println!("keyset id={} active={} unit={}", entry.id, entry.active, entry.unit);
    }
    Ok(())
}
