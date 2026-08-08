//! ComputeTx → COMPUTE_JSON_SPEC 线格式序列化。
//!
//! 节点 RPC 解析（`rabbitapi` `parse_compute_tx`）只接受 spec 格式：
//! 哈希一律 `0x` hex 字符串、owner/kind 为 tagged 枚举、bytes 为 hex 串。
//! rabbitcore 内部 serde 派生是原始字节数组表示，**不能直接提交**。
//! 任何 Rust 客户端/服务器提交交易前必须经 `compute_tx_to_spec_json` 转换。

use serde_json::{Value, json};

use crate::crypto::{Address, Hash};

use super::{
    Command, ComputeTx, ObjectKind, ObjectReadRef, ObjectId, Ownership, OutputProposal,
    ResourceValue, Script, TxSignature,
};

fn hash_hex(h: &Hash) -> String {
    format!("0x{}", hex::encode(h.as_bytes()))
}

fn addr_hex(a: &Address) -> String {
    format!("0x{}", hex::encode(a.as_bytes()))
}

fn bytes_hex(b: &[u8]) -> String {
    if b.is_empty() {
        "0x".to_string()
    } else {
        format!("0x{}", hex::encode(b))
    }
}

fn command_str(c: Command) -> &'static str {
    match c {
        Command::Transfer => "Transfer",
        Command::Invoke => "Invoke",
        Command::Mint => "Mint",
        Command::Burn => "Burn",
        Command::Anchor => "Anchor",
        Command::Reveal => "Reveal",
        Command::AgentTick => "AgentTick",
    }
}

fn kind_str(k: ObjectKind) -> &'static str {
    match k {
        ObjectKind::Asset => "Asset",
        ObjectKind::Code => "Code",
        ObjectKind::State => "State",
        ObjectKind::Capability => "Capability",
        ObjectKind::Agent => "Agent",
        ObjectKind::Anchor => "Anchor",
        ObjectKind::Ticket => "Ticket",
    }
}

fn owner_json(o: &Ownership) -> Value {
    match o {
        Ownership::Shared => json!({ "type": "Shared" }),
        Ownership::Address(a) => json!({ "type": "Address", "address": addr_hex(a) }),
        Ownership::Program(a) => json!({ "type": "Program", "address": addr_hex(a) }),
        Ownership::Ed25519(pk) => json!({ "type": "Ed25519", "public_key": bytes_hex(pk) }),
    }
}

fn script_json(s: &Script) -> Value {
    json!({ "vm": s.vm, "code": bytes_hex(&s.code) })
}

fn metadata_json(m: &[(String, Vec<u8>)]) -> Value {
    m.iter()
        .map(|(k, v)| json!({ "key": k, "value": bytes_hex(v) }))
        .collect()
}

fn resource_value_json(v: &ResourceValue) -> Value {
    match v {
        ResourceValue::Amount(amount) => json!({ "type": "Amount", "amount": amount }),
        ResourceValue::Data(data) => json!({ "type": "Data", "data": bytes_hex(data) }),
        ResourceValue::Ref(obj) => json!({ "type": "Ref", "object_id": hash_hex(&obj.0) }),
        ResourceValue::RefBatch(objs) => json!({
            "type": "RefBatch",
            "object_ids": objs.iter().map(|o| hash_hex(&o.0)).collect::<Vec<_>>(),
        }),
    }
}

fn resources_json(r: &[(Hash, ResourceValue)]) -> Value {
    r.iter()
        .map(|(asset_id, value)| {
            json!({ "asset_id": hash_hex(asset_id), "value": resource_value_json(value) })
        })
        .collect()
}

fn read_set_json(r: &ObjectReadRef) -> Value {
    json!({
        "output_id": hash_hex(&r.output_id.0),
        "domain_id": r.domain_id.0,
        "expected_version": r.expected_version.0,
    })
}

fn proposal_json(p: &OutputProposal) -> Value {
    json!({
        "output_id": hash_hex(&p.output_id.0),
        "object_id": hash_hex(&p.object_id.0),
        "domain_id": p.domain_id.0,
        "kind": kind_str(p.kind),
        "owner": owner_json(&p.owner),
        "predecessor": p.predecessor.map(|o| hash_hex(&o.0)),
        "version": p.version.0,
        "state": bytes_hex(&p.state),
        "state_root": p.state_root.as_ref().map(hash_hex),
        "resources": resources_json(&p.resources),
        "lock": script_json(&p.lock),
        "logic": p.logic.as_ref().map(script_json),
        "created_at": p.created_at,
        "ttl": p.ttl,
        "rent_reserve": p.rent_reserve,
        "flags": p.flags,
        "extensions": metadata_json(&p.extensions),
    })
}

fn witness_json(sigs: &[TxSignature], threshold: Option<u16>) -> Value {
    json!({
        "signatures": sigs.iter().map(|s| {
            json!({
                "scheme": "ed25519",
                "signature": bytes_hex(&s.bytes),
                "public_key": bytes_hex(s.public_key.as_deref().unwrap_or_default()),
            })
        }).collect::<Vec<_>>(),
        "threshold": threshold,
    })
}

/// 将内部 `ComputeTx` 转换为节点 RPC 接受的 spec JSON（与 `parse_compute_tx` 对称）。
pub fn compute_tx_to_spec_json(tx: &ComputeTx) -> Value {
    json!({
        "tx_id": hash_hex(&tx.tx_id.0),
        "domain_id": tx.domain_id.0,
        "command": command_str(tx.command),
        "input_set": tx.input_set.iter().map(|o| hash_hex(&o.0)).collect::<Vec<_>>(),
        "read_set": tx.read_set.iter().map(read_set_json).collect::<Vec<_>>(),
        "output_proposals": tx.output_proposals.iter().map(proposal_json).collect::<Vec<_>>(),
        "fee": tx.fee,
        "nonce": tx.nonce,
        "metadata": metadata_json(&tx.metadata),
        "payload": bytes_hex(&tx.payload),
        "deadline_unix_secs": tx.deadline_unix_secs,
        "chain_id": tx.chain_id,
        "network_id": tx.network_id,
        "witness": witness_json(&tx.witness.signatures, tx.witness.threshold),
        "max_fee": tx.max_fee,
        "priority_fee": tx.priority_fee,
        "gas_limit": tx.gas_limit,
    })
}

/// `ObjectId` 的 spec 格式（供外部查询构造，如 `rabbit_getObject`）。
pub fn object_id_to_hex(id: &ObjectId) -> String {
    hash_hex(&id.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::primitives::OutputId;

    #[test]
    fn empty_bytes_serialize_as_0x() {
        assert_eq!(bytes_hex(&[]), "0x");
        assert_eq!(bytes_hex(&[0xAB]), "0xab");
    }

    #[test]
    fn command_and_kind_roundtrip_names() {
        assert_eq!(command_str(Command::Mint), "Mint");
        assert_eq!(kind_str(ObjectKind::State), "State");
    }

    #[test]
    fn proposal_json_uses_hex_strings() {
        let proposal = OutputProposal {
            output_id: OutputId(Hash::from_bytes([1; 32])),
            object_id: ObjectId(Hash::from_bytes([2; 32])),
            domain_id: crate::compute::GAME_DOMAIN,
            kind: ObjectKind::State,
            owner: Ownership::Shared,
            predecessor: None,
            version: crate::compute::Version(1),
            state: vec![1, 2, 3],
            state_root: None,
            resources: vec![],
            lock: Script::default(),
            logic: None,
            created_at: 0,
            ttl: None,
            rent_reserve: None,
            flags: 0,
            extensions: vec![],
        };
        let v = proposal_json(&proposal);
        assert!(v["output_id"].as_str().unwrap().starts_with("0x"));
        assert_eq!(v["state"].as_str().unwrap(), "0x010203");
        assert_eq!(v["kind"], "State");
        assert_eq!(v["owner"]["type"], "Shared");
        assert!(v["predecessor"].is_null());
    }

    #[test]
    fn witness_json_uses_ed25519_fields() {
        let sig = TxSignature {
            scheme: super::super::SignatureScheme::Ed25519,
            bytes: vec![0xAB; 64],
            public_key: Some(vec![0xCD; 32]),
        };
        let v = witness_json(&[sig], None);
        assert_eq!(v["signatures"][0]["scheme"], "ed25519");
        assert_eq!(v["signatures"][0]["signature"].as_str().unwrap().len(), 130);
        assert!(v["threshold"].is_null());
    }
}
