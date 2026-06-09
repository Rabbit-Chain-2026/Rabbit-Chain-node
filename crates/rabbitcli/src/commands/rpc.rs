//! Shared JSON-RPC client helpers for rabbitcli commands.

use crate::Result;
use anyhow::{anyhow, Context};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    data: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

pub async fn rpc_call<T>(
    rpc_url: &str,
    rpc_token: Option<&str>,
    method: &str,
    params: serde_json::Value,
) -> Result<T>
where
    T: DeserializeOwned,
{
    let client = reqwest::Client::new();
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let mut request = client.post(rpc_url).json(&payload);
    if let Some(token) = rpc_token {
        request = request.bearer_auth(token);
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("failed to call rpc method `{method}`"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("rpc http status {} for `{}`", status, method));
    }

    let body: JsonRpcResponse<T> = response
        .json()
        .await
        .with_context(|| format!("failed to decode rpc response for `{method}`"))?;
    if let Some(error) = body.error {
        return Err(anyhow!(
            "{}",
            format_rpc_error(method, error.code, &error.message, error.data.as_ref())
        ));
    }

    body.result
        .ok_or_else(|| anyhow!("rpc `{}` missing result field", method))
}

fn format_rpc_error(
    method: &str,
    code: i64,
    message: &str,
    data: Option<&serde_json::Value>,
) -> String {
    if code == -32010 && method == "rabbit_submitComputeTx" {
        return "当前节点拒绝提交 compute 交易，请检查节点配置与交易内容。".to_string();
    }

    match data {
        Some(data) => format!(
            "rpc `{}` error {}: {} ({})",
            method,
            code,
            message,
            serde_json::to_string(data).unwrap_or_else(|_| data.to_string())
        ),
        None => format!("rpc `{}` error {}: {}", method, code, message),
    }
}

#[cfg(test)]
mod tests {
    use super::format_rpc_error;

    #[test]
    fn format_rpc_error_includes_error_data() {
        let message = format_rpc_error(
            "rabbit_submitComputeTx",
            -32602,
            "Invalid params",
            Some(&serde_json::json!({
                "code": "signature_owner_mismatch",
                "message": "signature does not authorize owner"
            })),
        );

        assert!(message.contains("Invalid params"));
        assert!(message.contains("signature_owner_mismatch"));
        assert!(message.contains("signature does not authorize owner"));
    }
}
