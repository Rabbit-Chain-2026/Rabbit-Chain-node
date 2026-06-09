//! Storage maintenance commands.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rabbitapi::rpc::ComputeBackend;
use rabbitcore::block::create_genesis_block;
use rabbitcore::compute::{
    execution::ObjectStore,
    object::{ObjectKind, Ownership, Script},
    primitives::{DomainId, ObjectId, OutputId, TxId, Version},
    ObjectOutput,
};
use rabbitcore::crypto::Hash;
use rabbitstore::archive::{
    ArchiveSegment, ArchiveSegmentWriter, ArchivedBlock, ArchivedComputeTxResult,
};
use rabbitstore::compute::{
    prune_compute_store, rebuild_compute_store, ComputePruneConfig, ComputePruneStats,
    ComputeRebuildStats,
};
use rabbitstore::db::{KeyValueDB, RedbDatabase, RocksDb, RocksDbCompression};
use rabbitstore::ComputeStore;

/// Rebuilds an existing compute database into the current storage format.
pub fn rebuild_compute_db(
    backend: ComputeBackend,
    path: &str,
    discard_backup: bool,
    dry_run: bool,
) -> Result<ComputeRebuildStats> {
    match backend {
        ComputeBackend::Mem => bail!("cannot rebuild in-memory compute backend"),
        ComputeBackend::RocksDb | ComputeBackend::Redb => {}
    }

    let source_path = Path::new(path);
    if !source_path.exists() {
        bail!("compute database path does not exist: {path}");
    }

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_millis();
    let target_path = format!("{path}.rebuild-{suffix}");
    let backup_path = format!("{path}.backup-{suffix}");

    ensure_absent(&target_path)?;
    ensure_absent(&backup_path)?;

    let (stats, source_entries, rebuilt_entries) = {
        let source = open_compute_backend(backend, path)?;
        let target = open_compute_backend(backend, &target_path)?;
        let source_entries = count_entries(source.as_ref())
            .with_context(|| format!("failed to count source compute database at {path}"))?;
        let stats = rebuild_compute_store(source.as_ref(), target.as_ref())
            .with_context(|| rebuild_failure_hint(path, &target_path))?;
        let rebuilt_entries = count_entries(target.as_ref()).with_context(|| {
            format!("failed to count rebuilt compute database at {target_path}")
        })?;
        if source_entries != stats.total_entries {
            bail!(
                "source entry count changed during rebuild: counted {source_entries}, scanned {}; stop the node before retrying",
                stats.total_entries
            );
        }
        if rebuilt_entries != stats.total_entries {
            bail!(
                "rebuilt entry count mismatch: source scanned {}, target contains {rebuilt_entries}; rebuilt database remains at {target_path}, remove it before retrying",
                stats.total_entries
            );
        }
        (stats, source_entries, rebuilt_entries)
    };

    if dry_run {
        remove_path(&target_path)
            .with_context(|| format!("failed to remove dry-run database at {target_path}"))?;
        println!("Compute DB rebuild dry run complete");
        print_rebuild_summary(
            backend,
            path,
            None,
            stats,
            source_entries,
            rebuilt_entries,
            None,
        );
        return Ok(stats);
    }

    fs::rename(path, &backup_path)
        .with_context(|| format!("failed to move old compute database to {backup_path}"))?;
    if let Err(err) = fs::rename(&target_path, path) {
        if let Err(rollback_err) = fs::rename(&backup_path, path) {
            bail!(
                "failed to install rebuilt compute database ({err}); rollback also failed ({rollback_err}); backup remains at {backup_path}, rebuilt database remains at {target_path}; inspect both paths before retrying"
            );
        }
        bail!(
            "failed to install rebuilt compute database: {err}; restored original database; rebuilt database remains at {target_path}, remove it before retrying"
        );
    }

    let installed_entries = {
        let installed = open_compute_backend(backend, path)?;
        count_entries(installed.as_ref())
            .with_context(|| format!("failed to count installed compute database at {path}"))?
    };
    if installed_entries != stats.total_entries {
        bail!(
            "installed entry count mismatch: source scanned {}, installed database contains {installed_entries}; backup remains at {backup_path}",
            stats.total_entries
        );
    }

    if discard_backup {
        remove_path(&backup_path)
            .with_context(|| format!("failed to remove backup at {backup_path}"))?;
    }

    println!("Compute DB rebuild complete");
    print_rebuild_summary(
        backend,
        path,
        Some(if discard_backup {
            "discarded".to_string()
        } else {
            backup_path
        }),
        stats,
        source_entries,
        rebuilt_entries,
        Some(installed_entries),
    );

    Ok(stats)
}

/// Prunes old compute hot-state entries according to an explicit retention profile.
pub fn prune_compute_db(
    backend: ComputeBackend,
    path: &str,
    retention_profile: &str,
    retention_window_secs: u64,
    now_unix_secs: Option<u64>,
    dry_run: bool,
) -> Result<ComputePruneStats> {
    match backend {
        ComputeBackend::Mem => bail!("cannot prune in-memory compute backend"),
        ComputeBackend::RocksDb | ComputeBackend::Redb => {}
    }

    if !Path::new(path).exists() {
        bail!("compute database path does not exist: {path}");
    }

    let retain_all = match retention_profile.to_ascii_lowercase().as_str() {
        "full" | "validator" | "mainnet" => false,
        "archive" | "explorer" | "retain-all" => true,
        other => bail!(
            "unsupported retention profile: {other}; expected full|validator|mainnet|archive|explorer|retain-all"
        ),
    };
    let now_unix_secs = match now_unix_secs {
        Some(value) => value,
        None => current_unix_secs()?,
    };

    let db = open_compute_backend(backend, path)?;
    let stats = prune_compute_store(
        db.as_ref(),
        ComputePruneConfig {
            retain_all,
            now_unix_secs,
            retention_window_secs,
            dry_run,
        },
    )
    .with_context(|| format!("failed to prune compute database at {path}"))?;

    println!(
        "Compute DB prune {}",
        if dry_run {
            "dry run complete"
        } else {
            "complete"
        }
    );
    println!("   backend: {}", backend.as_str());
    println!("   path: {path}");
    println!("   retention profile: {retention_profile}");
    println!("   retain all: {retain_all}");
    println!("   now unix secs: {now_unix_secs}");
    println!("   retention window secs: {retention_window_secs}");
    println!("   entries: {}", stats.total_entries);
    println!("   output entries: {}", stats.output_entries);
    println!("   tx result entries: {}", stats.tx_result_entries);
    println!(
        "   spent output candidates: {}",
        stats.spent_output_candidates
    );
    println!("   tx result candidates: {}", stats.tx_result_candidates);
    println!(
        "   latest entries deleted: {}",
        stats.latest_entries_deleted
    );
    println!("   deleted entries: {}", stats.deleted_entries);

    Ok(stats)
}

/// Generates a fixed workload and writes a disk-footprint comparison report.
pub fn benchmark_compute_storage(
    work_dir: &str,
    report_path: &str,
    outputs: u64,
    queries: u64,
    overwrite: bool,
) -> Result<()> {
    if outputs == 0 {
        bail!("outputs must be greater than zero");
    }
    if queries == 0 {
        bail!("queries must be greater than zero");
    }

    let work_dir = Path::new(work_dir);
    if work_dir.exists() {
        if overwrite {
            fs::remove_dir_all(work_dir).with_context(|| {
                format!("failed to remove existing work dir {}", work_dir.display())
            })?;
        } else {
            bail!(
                "work dir already exists: {}; pass --overwrite to replace it",
                work_dir.display()
            );
        }
    }
    fs::create_dir_all(work_dir)?;

    let workload = BenchmarkWorkload::new(outputs, queries);
    let legacy = run_legacy_json_lz4_benchmark(work_dir, &workload)?;
    let binary = run_binary_zstd_benchmark(work_dir, &workload, false)?;
    let pruned = run_binary_zstd_benchmark(work_dir, &workload, true)?;
    let archive = run_archive_segment_benchmark(work_dir, &workload)?;

    let report = render_storage_savings_report(&workload, &[legacy, binary, pruned, archive]);
    if let Some(parent) = Path::new(report_path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(report_path, report)?;
    println!("Storage savings report written to {report_path}");
    Ok(())
}

struct BenchmarkWorkload {
    outputs: u64,
    queries: u64,
    now_unix_secs: u64,
    retention_window_secs: u64,
}

impl BenchmarkWorkload {
    fn new(outputs: u64, queries: u64) -> Self {
        Self {
            outputs,
            queries,
            now_unix_secs: 1_000_000,
            retention_window_secs: 100,
        }
    }
}

struct BenchmarkScenario {
    name: &'static str,
    bytes: u64,
    restart_ms: Option<f64>,
    query_avg_us: Option<f64>,
    notes: &'static str,
}

fn run_legacy_json_lz4_benchmark(
    work_dir: &Path,
    workload: &BenchmarkWorkload,
) -> Result<BenchmarkScenario> {
    let path = work_dir.join("legacy-json-lz4");
    {
        let db = RocksDb::open_with_compression(
            path.to_str().context("legacy path is not utf8")?,
            RocksDbCompression::Lz4,
        )?;
        for i in 0..workload.outputs {
            let output = benchmark_output(i);
            db.put(&output_key(output.output_id), &serde_json::to_vec(&output)?)?;
            db.put(&latest_key(output.object_id), output.output_id.0.as_bytes())?;
            let tx_id = benchmark_tx_id(i);
            db.put(&tx_result_key(tx_id), benchmark_result_json(i).as_bytes())?;
        }
        db.flush()?;
    }
    let bytes = dir_size(&path)?;
    let restart_ms = measure_restart_ms(&path, RocksDbCompression::Lz4)?;
    let query_avg_us = measure_query_avg_us(&path, RocksDbCompression::Lz4, workload)?;
    Ok(BenchmarkScenario {
        name: "legacy JSON + RocksDB LZ4",
        bytes,
        restart_ms: Some(restart_ms),
        query_avg_us: Some(query_avg_us),
        notes: "baseline hot KV",
    })
}

fn run_binary_zstd_benchmark(
    work_dir: &Path,
    workload: &BenchmarkWorkload,
    prune: bool,
) -> Result<BenchmarkScenario> {
    let path = work_dir.join(if prune {
        "binary-zstd-pruned"
    } else {
        "binary-zstd"
    });
    {
        let db = Arc::new(RocksDb::open_with_compression(
            path.to_str().context("binary path is not utf8")?,
            RocksDbCompression::Zstd,
        )?);
        let store = ComputeStore::new(db.clone());
        for i in 0..workload.outputs {
            store
                .insert_output(benchmark_output(i))
                .map_err(|e| anyhow::anyhow!("insert benchmark output {i}: {e}"))?;
            store.put_tx_result(benchmark_tx_id(i), &benchmark_result_json(i))?;
        }
        if prune {
            prune_compute_store(
                db.as_ref(),
                ComputePruneConfig {
                    retain_all: false,
                    now_unix_secs: workload.now_unix_secs,
                    retention_window_secs: workload.retention_window_secs,
                    dry_run: false,
                },
            )?;
            db.compact_all();
        }
        db.flush()?;
    }
    let bytes = dir_size(&path)?;
    let restart_ms = measure_restart_ms(&path, RocksDbCompression::Zstd)?;
    let query_avg_us = measure_query_avg_us(&path, RocksDbCompression::Zstd, workload)?;
    Ok(BenchmarkScenario {
        name: if prune {
            "binary + RocksDB ZSTD + pruning"
        } else {
            "binary + RocksDB ZSTD"
        },
        bytes,
        restart_ms: Some(restart_ms),
        query_avg_us: Some(query_avg_us),
        notes: if prune {
            "old spent outputs and old tx results pruned"
        } else {
            "current hot KV format"
        },
    })
}

fn run_archive_segment_benchmark(
    work_dir: &Path,
    workload: &BenchmarkWorkload,
) -> Result<BenchmarkScenario> {
    let path = work_dir.join("archive-segments");
    let writer = ArchiveSegmentWriter::new(&path);
    let mut segment = ArchiveSegment::new(0, workload.outputs.saturating_sub(1), 1_000_001);
    segment
        .blocks
        .push(ArchivedBlock::from_block(create_genesis_block()));
    for i in 0..workload.outputs {
        let output = benchmark_output(i);
        if output.spent {
            segment.compute_outputs.push(output);
            segment.compute_tx_results.push(ArchivedComputeTxResult {
                tx_id: benchmark_tx_id(i),
                result_json: benchmark_result_json(i),
            });
        }
    }
    writer.write_segment(&segment)?;
    Ok(BenchmarkScenario {
        name: "archive segment ZSTD",
        bytes: dir_size(&path)?,
        restart_ms: None,
        query_avg_us: None,
        notes: "consensus-external finalized history",
    })
}

fn render_storage_savings_report(
    workload: &BenchmarkWorkload,
    scenarios: &[BenchmarkScenario],
) -> String {
    let baseline = scenarios.first().map(|s| s.bytes).unwrap_or(1).max(1);
    let mut out = String::new();
    out.push_str("# RabbitChain Storage Savings Report\n\n");
    out.push_str("Generated by `rabbitchain storage benchmark-compute-db`.\n\n");
    out.push_str("## Fixed Workload\n\n");
    out.push_str(&format!("- Compute outputs: {}\n", workload.outputs));
    out.push_str(&format!("- Compute tx results: {}\n", workload.outputs));
    out.push_str(&format!(
        "- Point lookups per hot DB: {}\n",
        workload.queries
    ));
    out.push_str(&format!(
        "- Pruning window: {} seconds at now={}\n\n",
        workload.retention_window_secs, workload.now_unix_secs
    ));
    out.push_str("## Results\n\n");
    out.push_str("| Scenario | Size bytes | Size MiB | Size GiB | Saved vs baseline | Restart ms | Avg get us | Notes |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|---:|---|\n");
    for scenario in scenarios {
        let saved = 1.0 - (scenario.bytes as f64 / baseline as f64);
        out.push_str(&format!(
            "| {} | {} | {:.3} | {:.6} | {:.2}% | {} | {} | {} |\n",
            scenario.name,
            scenario.bytes,
            scenario.bytes as f64 / 1024.0 / 1024.0,
            scenario.bytes as f64 / 1024.0 / 1024.0 / 1024.0,
            saved * 100.0,
            scenario
                .restart_ms
                .map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "n/a".to_string()),
            scenario
                .query_avg_us
                .map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "n/a".to_string()),
            scenario.notes
        ));
    }
    out.push_str("\n## Interpretation\n\n");
    out.push_str("- Hot validators should compare against `binary + RocksDB ZSTD + pruning`.\n");
    out.push_str("- Archive/explorer nodes should keep archive segments and can rebuild analytics tables from them.\n");
    out.push_str("- Hashes, signatures, and public keys are high-entropy data, so most savings come from removing JSON overhead and pruning finalized history.\n");
    out
}

fn benchmark_output(i: u64) -> ObjectOutput {
    let old_half = i % 2 == 0;
    ObjectOutput {
        output_id: OutputId(index_hash(i.saturating_add(1))),
        object_id: ObjectId(index_hash(i.saturating_add(10_000_000))),
        version: Version(1),
        domain_id: DomainId(0),
        kind: ObjectKind::State,
        owner: Ownership::Shared,
        predecessor: None,
        state: benchmark_state(i),
        state_root: None,
        resources: vec![],
        lock: Script::default(),
        logic: None,
        created_at: if old_half { 10 } else { 999_950 },
        ttl: None,
        rent_reserve: None,
        flags: 0,
        extensions: vec![],
        spent: old_half,
    }
}

fn benchmark_state(i: u64) -> Vec<u8> {
    let mut state = Vec::with_capacity(96);
    for n in 0..12u64 {
        state.extend_from_slice(&i.wrapping_mul(31).wrapping_add(n).to_be_bytes());
    }
    state
}

fn benchmark_tx_id(i: u64) -> TxId {
    TxId(index_hash(i.saturating_add(20_000_000)))
}

fn benchmark_result_json(i: u64) -> String {
    let submitted_at = if i % 2 == 0 { 10 } else { 999_950 };
    serde_json::json!({
        "ok": true,
        "tx_id": format!("0x{}", hex::encode(index_hash(i.saturating_add(20_000_000)).as_bytes())),
        "consumed_inputs": i % 3,
        "read_objects": i % 5,
        "created_outputs": 1,
        "submitted_at_unix": submitted_at
    })
    .to_string()
}

fn index_hash(i: u64) -> Hash {
    let mut bytes = [0u8; 32];
    bytes[24..32].copy_from_slice(&i.to_be_bytes());
    Hash::from_bytes(bytes)
}

fn output_key(output_id: OutputId) -> Vec<u8> {
    let mut key = b"compute:output:".to_vec();
    key.extend_from_slice(output_id.0.as_bytes());
    key
}

fn latest_key(object_id: ObjectId) -> Vec<u8> {
    let mut key = b"compute:latest:".to_vec();
    key.extend_from_slice(object_id.0.as_bytes());
    key
}

fn tx_result_key(tx_id: TxId) -> Vec<u8> {
    let mut key = b"compute:txresult:".to_vec();
    key.extend_from_slice(tx_id.0.as_bytes());
    key
}

fn dir_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total.saturating_add(dir_size(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn measure_restart_ms(path: &Path, compression: RocksDbCompression) -> Result<f64> {
    let start = Instant::now();
    let db =
        RocksDb::open_with_compression(path.to_str().context("db path is not utf8")?, compression)?;
    db.get(&output_key(OutputId(index_hash(1))))?;
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

fn measure_query_avg_us(
    path: &Path,
    compression: RocksDbCompression,
    workload: &BenchmarkWorkload,
) -> Result<f64> {
    let db =
        RocksDb::open_with_compression(path.to_str().context("db path is not utf8")?, compression)?;
    let start = Instant::now();
    for i in 0..workload.queries {
        let idx = i.wrapping_mul(7_919) % workload.outputs;
        db.get(&output_key(OutputId(index_hash(idx.saturating_add(1)))))?;
    }
    Ok(start.elapsed().as_secs_f64() * 1_000_000.0 / workload.queries as f64)
}

fn open_compute_backend(backend: ComputeBackend, path: &str) -> Result<Arc<dyn KeyValueDB>> {
    match backend {
        ComputeBackend::Mem => bail!("cannot open in-memory compute backend for rebuild"),
        ComputeBackend::RocksDb => {
            let db =
                RocksDb::open(path).with_context(|| format!("failed to open rocksdb at {path}"))?;
            Ok(Arc::new(db))
        }
        ComputeBackend::Redb => {
            let db = RedbDatabase::open(path)
                .with_context(|| format!("failed to open redb at {path}"))?;
            Ok(Arc::new(db))
        }
    }
}

fn count_entries(db: &dyn KeyValueDB) -> Result<u64> {
    let mut count = 0u64;
    db.for_each_prefix(b"", &mut |_, _| {
        count = count.saturating_add(1);
        Ok(())
    })?;
    Ok(count)
}

fn ensure_absent(path: &str) -> Result<()> {
    if Path::new(path).exists() {
        bail!("refusing to overwrite existing path: {path}");
    }
    Ok(())
}

fn rebuild_failure_hint(path: &str, target_path: &str) -> String {
    format!(
        "failed to rebuild compute database from {path}; temporary database may remain at {target_path}, inspect or remove it before retrying"
    )
}

fn current_unix_secs() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs())
}

fn print_rebuild_summary(
    backend: ComputeBackend,
    path: &str,
    backup: Option<String>,
    stats: ComputeRebuildStats,
    source_entries: u64,
    rebuilt_entries: u64,
    installed_entries: Option<u64>,
) {
    println!("   backend: {}", backend.as_str());
    println!("   path: {path}");
    println!(
        "   backup: {}",
        backup.unwrap_or_else(|| "not created".to_string())
    );
    println!("   source entries: {source_entries}");
    println!("   rebuilt entries: {rebuilt_entries}");
    if let Some(installed_entries) = installed_entries {
        println!("   installed entries: {installed_entries}");
    }
    println!("   output entries: {}", stats.output_entries);
    println!("   legacy json outputs: {}", stats.legacy_json_outputs);
    println!("   binary outputs: {}", stats.binary_outputs);
    println!("   tx result entries: {}", stats.tx_result_entries);
    println!(
        "   legacy json tx results: {}",
        stats.legacy_json_tx_results
    );
    println!("   binary tx results: {}", stats.binary_tx_results);
    println!("   copied entries: {}", stats.copied_entries);
    println!("   rewritten entries: {}", stats.rewritten_entries);
}

fn remove_path(path: &str) -> Result<()> {
    let metadata = fs::metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}
