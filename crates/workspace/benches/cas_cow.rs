//! Benchmarks for agent-side storage: CAS put/materialize, Copy-on-Write
//! workspace cloning, and manifest diffing (cold vs warm workspace).
//!
//! Run with `cargo bench -p workspace` or `scripts/run_benchmarks.sh`.
//!
//! Note: CoW and CAS materialize exercise the best path available on the
//! host filesystem (APFS `clonefile` / Linux `FICLONE` ioctl, falling back
//! to hardlink or full copy). Numbers are therefore filesystem-dependent —
//! record the filesystem type when publishing results.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use criterion::{BatchSize, Criterion, Throughput};
use protocol::{FileEntry, ManifestPayload};
use tempfile::TempDir;
use workspace::cas::CasStore;
use workspace::diff_manifests;

/// Deterministic pseudo-random bytes.
fn pseudo_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xff) as u8);
    }
    out
}

/// Create a source tree: 100 files x 4 KiB in nested dirs.
fn build_tree(root: &Path, seed: u64) -> Vec<PathBuf> {
    let mut written = Vec::new();
    for i in 0..100 {
        let dir = root.join("src").join(if i % 2 == 0 { "a" } else { "b" });
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("f_{i}.rs"));
        fs::write(&file, pseudo_bytes(2048, seed + i as u64)).unwrap();
        written.push(file);
    }
    written
}

fn manifest_from(root: &Path) -> ManifestPayload {
    let scanned = fileset::scan(root, &[]).expect("scan");
    let mut files: Vec<FileEntry> = scanned
        .values()
        .map(|m| FileEntry {
            path: m.path.clone(),
            hash: m.hash.clone(),
            size: m.size,
            mode: m.mode,
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    ManifestPayload { files }
}

fn bench_cas_put(c: &mut Criterion, size: usize, name: &str) {
    let mut group = c.benchmark_group("cas");
    group.throughput(Throughput::Bytes(size as u64));
    group.bench_function(format!("cas_put_{name}"), |b| {
        b.iter_batched(
            || {
                let base = TempDir::new().expect("cas base");
                let src = base.path().join("src.bin");
                fs::write(&src, pseudo_bytes(size, 42)).unwrap();
                (base, src)
            },
            |(base, src)| {
                let store = CasStore::new(base.path());
                let sha = fileset::hash_file(&src).expect("hash");
                store.put_file(&sha, &src).expect("put")
            },
            BatchSize::LargeInput,
        )
    });
    group.finish();
}

fn bench_cas_materialize(c: &mut Criterion, size: usize, name: &str) {
    // Pre-populate one CAS store, then measure hydration into fresh paths.
    let base = TempDir::new().expect("cas base");
    let store = CasStore::new(base.path());
    let src = base.path().join("src.bin");
    fs::write(&src, pseudo_bytes(size, 42)).unwrap();
    let sha = fileset::hash_file(&src).expect("hash");
    store.put_file(&sha, &src).expect("put");

    let mut group = c.benchmark_group("cas");
    group.throughput(Throughput::Bytes(size as u64));
    group.bench_function(format!("cas_materialize_{name}"), |b| {
        let seq = Cell::new(0u64);
        b.iter_batched(
            || {
                let dir = base.path().join(format!("dest{}", seq.get()));
                seq.set(seq.get() + 1);
                // Untimed: clear any destination left by a previous pass.
                let _ = fs::remove_file(&dir);
                dir
            },
            |dest| store.materialize_to(&sha, &dest).expect("materialize"),
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

fn bench_cow_clone_dir(c: &mut Criterion) {
    let seed_dir = TempDir::new().expect("seed");
    build_tree(seed_dir.path(), 7);
    let parent = TempDir::new().expect("parent");

    let mut group = c.benchmark_group("cow");
    group.bench_function("cow_clone_dir_100_files_200k", |b| {
        let seq = Cell::new(0u64);
        b.iter_batched(
            || {
                let dst = parent.path().join(format!("it{}", seq.get()));
                seq.set(seq.get() + 1);
                // Untimed: a previous warmup/measurement pass may have left
                // this destination behind; cow_clone_dir refuses to overwrite.
                let _ = fs::remove_dir_all(&dst);
                dst
            },
            |dst| workspace::cow::cow_clone_dir(seed_dir.path(), &dst).expect("clone"),
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

fn bench_diff_manifests(c: &mut Criterion) {
    let fixture = TempDir::new().expect("fixture");
    build_tree(fixture.path(), 777);
    let manifest = manifest_from(fixture.path());

    // Cold: empty workspace — agent must scan nothing but still walk + hash remote dir.
    let cold_ws = TempDir::new().expect("cold ws");

    // Warm: workspace already mirrors the fixture — hashes must all match.
    let warm_ws = TempDir::new().expect("warm ws");
    for rel in &manifest.files {
        let src = fixture.path().join(&rel.path);
        let dst = warm_ws.path().join(&rel.path);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(src, dst).unwrap();
    }

    let mut group = c.benchmark_group("diff");
    group.bench_function("diff_manifests_cold_workspace", |b| {
        b.iter(|| diff_manifests(cold_ws.path(), &manifest, &[]).expect("diff"))
    });
    group.bench_function("diff_manifests_warm_workspace_noop", |b| {
        b.iter(|| diff_manifests(warm_ws.path(), &manifest, &[]).expect("diff"))
    });
    group.finish();
}

fn main() {
    let mut crit = Criterion::default();
    bench_cas_put(&mut crit, 64 * 1024, "64k");
    bench_cas_put(&mut crit, 256 * 1024, "256k");
    bench_cas_materialize(&mut crit, 64 * 1024, "64k");
    bench_cas_materialize(&mut crit, 256 * 1024, "256k");
    bench_cow_clone_dir(&mut crit);
    bench_diff_manifests(&mut crit);
    crit.final_summary();
}
