//! Account command implementation.

use crate::commands::rpc::rpc_call;
use crate::commands::wallet::{self, WalletCommand, WalletScheme};
use crate::{AccountAction, Result};
use serde_json::json;

pub async fn handle_account(
    action: AccountAction,
    data_dir: &str,
    rpc_url: &str,
    rpc_token: Option<&str>,
) -> Result<()> {
    match action {
        AccountAction::New {
            name,
            scheme,
            passphrase,
        } => {
            wallet::handle_wallet(
                data_dir,
                WalletCommand::New {
                    name,
                    scheme: parse_wallet_scheme(&scheme)?,
                    passphrase,
                },
            )
            .await?;
        }
        AccountAction::List => {
            wallet::handle_wallet(data_dir, WalletCommand::List).await?;
        }
        AccountAction::Balance { address } => {
            let account_info = rpc_call::<serde_json::Value>(
                rpc_url,
                rpc_token,
                "rabbit_getAccount",
                json!([address.clone()]),
            )
            .await?;

            println!("rpc_url: {}", rpc_url);
            println!("address: {}", address);
            println!(
                "account: {}",
                serde_json::to_string_pretty(&account_info)
                    .unwrap_or_else(|_| account_info.to_string())
            );
        }
    }

    Ok(())
}

fn parse_wallet_scheme(value: &str) -> Result<WalletScheme> {
    match value.to_ascii_lowercase().as_str() {
        "ed25519" => Ok(WalletScheme::Ed25519),
        other => anyhow::bail!("unsupported wallet scheme: {other}"),
    }
}
