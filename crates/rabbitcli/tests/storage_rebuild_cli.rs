use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;
use rabbitapi::rpc::ComputeBackend;
use rabbitcore::compute::{
    object::{ObjectKind, Ownership, Script},
    primitives::{DomainId, ObjectId, OutputId, TxId, Version},
    ObjectOutput,
};
use rabbitcore::crypto::Hash;
use rabbitstore::db::{KeyValueDB, RedbDatabase, RocksDb};

const OUTPUT_BINARY_MAGIC: &[u8; 4] = b"ZCO1";
const TX_RESULT_BINARY_MAGIC: &[u8; 4] = b"ZCR1";

#[test]
fn storage_rebuild_compute_db_dry_run_does_not_replace_rocksdb_or_redb() {
    for backend in [BackendCase::rocksdb(), BackendCase::redb()] {
        let dir = tempdir().expect("create temp dir");
        let path = backend.path(dir.path());
        let (output_key, tx_key) = seed_legacy_compute_db(backend.kind, &path);

        let output = run_rabbitchain([
            "storage",
            "rebuild-compute-db",
            "--dry-run",
            "--compute-backend",
            backend.name,
            "--compute-db-path",
            path.to_str().expect("utf8 path"),
        ]);
        assert_success(&output);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Compute DB rebuild dry run complete"));
        assert!(stdout.contains("backup: not created"));

        let db = open_backend(backend.kind, &path);
        let raw_output = db.get(&output_key).expect("get output").expect("output");
        assert!(!raw_output.starts_with(OUTPUT_BINARY_MAGIC));
        assert_eq!(
            db.get(&tx_key).expect("get tx result"),
            Some(br#"{"ok":true}"#.to_vec())
        );
        assert_no_sibling_path_with(&path, "backup-");
        assert_no_sibling_path_with(&path, "rebuild-");
    }
}

#[test]
fn storage_rebuild_compute_db_replaces_rocksdb_and_redb_with_backup() {
    for backend in [BackendCase::rocksdb(), BackendCase::redb()] {
        let dir = tempdir().expect("create temp dir");
        let path = backend.path(dir.path());
        let (output_key, tx_key) = seed_legacy_compute_db(backend.kind, &path);

        let output = run_rabbitchain([
            "storage",
            "rebuild-compute-db",
            "--compute-backend",
            backend.name,
            "--compute-db-path",
            path.to_str().expect("utf8 path"),
        ]);
        assert_success(&output);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Compute DB rebuild complete"));
        assert!(stdout.contains("installed entries: 2"));

        let db = open_backend(backend.kind, &path);
        let raw_output = db.get(&output_key).expect("get output").expect("output");
        assert!(raw_output.starts_with(OUTPUT_BINARY_MAGIC));
        let raw_tx_result = db.get(&tx_key).expect("get tx result").expect("tx result");
        assert!(raw_tx_result.starts_with(TX_RESULT_BINARY_MAGIC));
        assert_sibling_path_with(&path, "backup-");
        assert_no_sibling_path_with(&path, "rebuild-");
    }
}

#[test]
fn storage_prune_compute_db_prunes_rocksdb_and_redb() {
    for backend in [BackendCase::rocksdb(), BackendCase::redb()] {
        let dir = tempdir().expect("create temp dir");
        let path = backend.path(dir.path());
        let (output_key, tx_key) = seed_prunable_compute_db(backend.kind, &path);

        let output = run_rabbitchain([
            "storage",
            "prune-compute-db",
            "--compute-backend",
            backend.name,
            "--compute-db-path",
            path.to_str().expect("utf8 path"),
            "--retention-window-secs",
            "20",
            "--now-unix-secs",
            "120",
        ]);
        assert_success(&output);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Compute DB prune complete"));
        assert!(stdout.contains("deleted entries: 2"));

        let db = open_backend(backend.kind, &path);
        assert_eq!(db.get(&output_key).expect("get output"), None);
        assert_eq!(db.get(&tx_key).expect("get tx result"), None);
    }
}

#[derive(Clone, Copy)]
struct BackendCase {
    name: &'static str,
    kind: ComputeBackend,
    file_name: &'static str,
}

impl BackendCase {
    fn rocksdb() -> Self {
        Self {
            name: "rocksdb",
            kind: ComputeBackend::RocksDb,
            file_name: "compute-rocksdb",
        }
    }

    fn redb() -> Self {
        Self {
            name: "redb",
            kind: ComputeBackend::Redb,
            file_name: "compute-redb.db",
        }
    }

    fn path(self, parent: &Path) -> PathBuf {
        parent.join(self.file_name)
    }
}

fn seed_legacy_compute_db(backend: ComputeBackend, path: &Path) -> (Vec<u8>, Vec<u8>) {
    let output = sample_output();
    let tx_id = TxId(Hash::from_bytes([7; 32]));
    let output_key = output_key(output.output_id);
    let tx_key = tx_result_key(tx_id);

    {
        let db = open_backend(backend, path);
        db.put(
            &output_key,
            &serde_json::to_vec(&output).expect("encode legacy json"),
        )
        .expect("put output");
        db.put(&tx_key, br#"{"ok":true}"#).expect("put tx result");
    }

    (output_key, tx_key)
}

fn seed_prunable_compute_db(backend: ComputeBackend, path: &Path) -> (Vec<u8>, Vec<u8>) {
    let mut output = sample_output();
    output.created_at = 10;
    output.spent = true;
    let tx_id = TxId(Hash::from_bytes([14; 32]));
    let output_key = output_key(output.output_id);
    let tx_key = tx_result_key(tx_id);

    {
        let db = open_backend(backend, path);
        db.put(&output_key, &prefixed_output(&output))
            .expect("put output");
        db.put(&tx_key, &prefixed_tx_result(10))
            .expect("put tx result");
    }

    (output_key, tx_key)
}

fn open_backend(backend: ComputeBackend, path: &Path) -> Box<dyn KeyValueDB> {
    let path = path.to_str().expect("utf8 path");
    match backend {
        ComputeBackend::Mem => panic!("mem backend is not used in CLI rebuild tests"),
        ComputeBackend::RocksDb => Box::new(RocksDb::open(path).expect("open rocksdb")),
        ComputeBackend::Redb => Box::new(RedbDatabase::open(path).expect("open redb")),
    }
}

fn prefixed_output(output: &ObjectOutput) -> Vec<u8> {
    let mut encoded = OUTPUT_BINARY_MAGIC.to_vec();
    encoded.extend_from_slice(&bincode::serialize(output).expect("encode output"));
    encoded
}

fn prefixed_tx_result(submitted_at_unix: u64) -> Vec<u8> {
    #[derive(serde::Serialize)]
    struct TestEnvelope {
        result_json: String,
    }

    let result_json = serde_json::json!({
        "ok": true,
        "submitted_at_unix": submitted_at_unix
    })
    .to_string();
    let mut encoded = TX_RESULT_BINARY_MAGIC.to_vec();
    encoded.extend_from_slice(
        &bincode::serialize(&TestEnvelope { result_json }).expect("encode tx result"),
    );
    encoded
}

fn sample_output() -> ObjectOutput {
    ObjectOutput {
        output_id: OutputId(Hash::from_bytes([1; 32])),
        object_id: ObjectId(Hash::from_bytes([2; 32])),
        version: Version(1),
        domain_id: DomainId(0),
        kind: ObjectKind::State,
        owner: Ownership::Shared,
        predecessor: None,
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
        spent: false,
    }
}

fn output_key(output_id: OutputId) -> Vec<u8> {
    let mut key = b"compute:output:".to_vec();
    key.extend_from_slice(output_id.0.as_bytes());
    key
}

fn tx_result_key(tx_id: TxId) -> Vec<u8> {
    let mut key = b"compute:txresult:".to_vec();
    key.extend_from_slice(tx_id.0.as_bytes());
    key
}

fn run_rabbitchain<const N: usize>(args: [&str; N]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rabbitchain"))
        .args(args)
        .output()
        .expect("run rabbitchain")
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_sibling_path_with(path: &Path, marker: &str) {
    assert!(
        sibling_path_with(path, marker).is_some(),
        "expected sibling path containing {marker}"
    );
}

fn assert_no_sibling_path_with(path: &Path, marker: &str) {
    assert!(
        sibling_path_with(path, marker).is_none(),
        "unexpected sibling path containing {marker}"
    );
}

fn sibling_path_with(path: &Path, marker: &str) -> Option<PathBuf> {
    let parent = path.parent().expect("path parent");
    let file_name = path.file_name().expect("file name").to_string_lossy();
    fs::read_dir(parent)
        .expect("read parent")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|candidate| {
            candidate
                .file_name()
                .map(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with(file_name.as_ref()) && name.contains(marker)
                })
                .unwrap_or(false)
        })
}
