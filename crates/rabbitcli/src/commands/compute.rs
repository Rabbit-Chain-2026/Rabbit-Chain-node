//! Compute command implementation.

use crate::commands::rpc::rpc_call;
use crate::{ComputeAction, Result};
use anyhow::Context;
use serde_json::json;
use std::fs;
use rabbitapi::rpc::canonicalize_compute_tx_json;

pub async fn handle_compute(
    action: ComputeAction,
    _data_dir: &str,
    rpc_url: &str,
    rpc_token: Option<&str>,
) -> Result<()> {
    match action {
        ComputeAction::Get { tx_id } => {
            let result = rpc_call::<serde_json::Value>(
                rpc_url,
                rpc_token,
                "rabbit_getComputeTxResult",
                json!([tx_id.clone()]),
            )
            .await?;
            println!("rpc_url: {}", rpc_url);
            println!("tx_id: {}", tx_id);
            println!(
                "result: {}",
                serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
            );
        }
        ComputeAction::Send { tx_file } => {
            let raw = fs::read_to_string(&tx_file)
                .with_context(|| format!("failed to read tx file `{}`", tx_file))?;
            let tx_value: serde_json::Value = serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse tx json from `{}`", tx_file))?;

            if !tx_value.is_object() {
                anyhow::bail!("compute payload must be a JSON object");
            }

            let tx_value = canonicalize_compute_tx_json(tx_value).map_err(anyhow::Error::from)?;
            let tx_id = tx_value
                .get("tx_id")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing>");

            let result = rpc_call::<serde_json::Value>(
                rpc_url,
                rpc_token,
                "rabbit_submitComputeTx",
                json!([tx_value.clone()]),
            )
            .await?;

            println!("rpc_url: {}", rpc_url);
            println!("tx_file: {}", tx_file);
            println!("canonical_tx_id: {}", tx_id);
            println!(
                "submit_result: {}",
                serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::handle_compute;
    use crate::ComputeAction;
    use tempfile::TempDir;

    #[tokio::test]
    async fn compute_send_rejects_non_object_payload_before_rpc() {
        let temp = TempDir::new().expect("temp dir");
        let tx_file = temp.path().join("bad.json");
        std::fs::write(&tx_file, "[1,2,3]").expect("write tx file");

        let err = handle_compute(
            ComputeAction::Send {
                tx_file: tx_file.display().to_string(),
            },
            temp.path().to_str().expect("temp path"),
            "http://127.0.0.1:1",
            None,
        )
        .await
        .expect_err("non-object payload should fail");

        assert!(err
            .to_string()
            .contains("compute payload must be a JSON object"));
    }

    #[tokio::test]
    async fn compute_send_canonicalizes_tx_id_before_rpc() {
        let temp = TempDir::new().expect("temp dir");
        let tx_file = temp.path().join("tx.json");
        std::fs::write(
            &tx_file,
            r#"{
  "tx_id": "0x1111111111111111111111111111111111111111111111111111111111111111",
  "domain_id": 0,
  "command": "Mint",
  "input_set": [],
  "read_set": [],
  "output_proposals": [
    {
      "output_id": "0x1212121212121212121212121212121212121212121212121212121212121212",
      "object_id": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "domain_id": 0,
      "kind": "Asset",
      "owner": { "type": "Shared" },
      "version": 1,
      "state": "0x",
      "resources": [],
      "metadata": []
    }
  ],
  "fee": 0,
  "nonce": 777,
  "metadata": [],
  "payload": "0x",
  "witness": {
    "signatures": [
      {
        "scheme": "ed25519",
        "signature": "0x0dbec1ca9445527bfb10af41e2bcf16665776ed6c012e3fdd1e3e08aa0db253ccb240e1e9aad4ac64d37283ee2cccee3f8a19cb3ef4d0b72ea44c3f60fba710e",
        "public_key": "0x8a88e3dd7409f195fd52db2d3cba5d72ca670bf1d94121b3b1a4547075c4cb78"
      }
    ]
  }
}"#,
        )
        .expect("write tx file");

        let err = handle_compute(
            ComputeAction::Send {
                tx_file: tx_file.display().to_string(),
            },
            temp.path().to_str().expect("temp path"),
            "http://127.0.0.1:1",
            None,
        )
        .await
        .expect_err("rpc should fail after local canonicalization");

        let msg = err.to_string();
        assert!(msg.contains("failed to call rpc method `rabbit_submitComputeTx`"));
        assert!(!msg.contains("Invalid params"));
    }
}
