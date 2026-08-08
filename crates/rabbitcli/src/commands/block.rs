//! Block query commands.

use crate::commands::rpc::rpc_call;
use crate::{BlockAction, Result};
use anyhow::Context;
use serde_json::json;

pub async fn handle_block(
    action: BlockAction,
    rpc_url: &str,
    rpc_token: Option<&str>,
) -> Result<()> {
    match action {
        BlockAction::Latest => {
            let block = rpc_call::<serde_json::Value>(
                rpc_url,
                rpc_token,
                "rabbit_getLatestBlock",
                json!([]),
            )
            .await?;

            println!("rpc_url: {}", rpc_url);
            println!(
                "block: {}",
                serde_json::to_string_pretty(&block).unwrap_or_else(|_| block.to_string())
            );
        }
        BlockAction::Height => {
            let block = rpc_call::<serde_json::Value>(
                rpc_url,
                rpc_token,
                "rabbit_getLatestBlock",
                json!([]),
            )
            .await?;
            let height = latest_block_height(&block)?;

            println!("{height}");
        }
        BlockAction::Get { number } => {
            let number_hex = format!("0x{number:x}");
            let block = rpc_call::<serde_json::Value>(
                rpc_url,
                rpc_token,
                "rabbit_getBlockByNumber",
                json!([number_hex]),
            )
            .await?;

            println!("rpc_url: {}", rpc_url);
            println!("number: {}", number);
            println!(
                "block: {}",
                serde_json::to_string_pretty(&block).unwrap_or_else(|_| block.to_string())
            );
        }
    }

    Ok(())
}

fn latest_block_height(block: &serde_json::Value) -> Result<u64> {
    let height = block
        .get("number")
        .and_then(|value| value.as_str())
        .context("rpc response missing block number")?;
    let height = height
        .strip_prefix("0x")
        .unwrap_or(height)
        .trim_start_matches('0');

    if height.is_empty() {
        return Ok(0);
    }

    u64::from_str_radix(height, 16).context("rpc response block number was not valid hex")
}

#[cfg(test)]
mod tests {
    use super::latest_block_height;

    #[test]
    fn latest_block_height_parses_hex_string() {
        let block = serde_json::json!({
            "number": "0x2a"
        });

        assert_eq!(latest_block_height(&block).unwrap(), 42);
    }

    #[test]
    fn latest_block_height_handles_zero() {
        let block = serde_json::json!({
            "number": "0x0"
        });

        assert_eq!(latest_block_height(&block).unwrap(), 0);
    }
}
