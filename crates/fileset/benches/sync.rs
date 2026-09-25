//! Benchmarks for the local side of the sync pipeline: scan+hash, tar packing
//! (gzip vs zstd), and tar unpacking.
//!
//! Run with `cargo bench -p fileset` or `scripts/run_benchmarks.sh`.
//! Fixtures are deterministic pseudo-random data so numbers are comparable
//! across runs on the same machine.
//!
//! Fixture: 300 "source" files (1–4 KiB) across nested dirs + 20 "asset"
//! files (64 KiB each). Total payload ≈ 2.4 MiB.

use std::fs;
use std::path::Path;

use criterion::{BatchSize, Criterion, Throughput};
use fileset::tar::CompressionAlgo;
use tempfile::TempDir;

/// Build a deterministic source tree:
/// - 300 "source" files (1–4 KiB) across nested dirs
/// - 20 "asset" files (64 KiB each)
///
/// Total payload ≈ 2.4 MiB.
fn build_fixture(root: &Path) {
    let mut state: u64 = 0x1234_5678_9abc_def0;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    for i in 0..300 {
        let dir = root
            .join("src")
            .join(if i % 3 == 0 { "lib" } else { "mod" });
        fs::create_dir_all(&dir).unwrap();
        let size = 1024 + (next() % 3072) as usize;
        let content = (0..size / 8)
            .map(|_| format!("{:08x} ", next()))
            .collect::<String>();
        fs::write(dir.join(format!("file_{i}.ts")), content).unwrap();
    }

    let assets = root.join("assets");
    fs::create_dir_all(&assets).unwrap();
    for i in 0..20 {
        let size = 64 * 1024;
        let content = (0..size / 8)
            .map(|_| format!("{:08x}", next()))
            .collect::<String>();
        fs::write(assets.join(format!("asset_{i}.bin")), content).unwrap();
    }

    // A couple of files that the ignore engine should skip.
    fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    fs::write(
        root.join("node_modules/pkg/index.js"),
        "module.exports = 1;",
    )
    .unwrap();
}

fn fixture_paths(root: &Path) -> Vec<String> {
    let scanned = fileset::scan(root, &[]).expect("scan fixture");
    let mut paths: Vec<String> = scanned.into_keys().collect();
    paths.sort();
    paths
}

fn bench_scan_hash(c: &mut Criterion, root: &Path) {
    let mut group = c.benchmark_group("sync");
    let n_files = fixture_paths(root).len();
    group.throughput(Throughput::Elements(n_files as u64));
    group.bench_function("scan_and_hash_320_files", |b| {
        b.iter(|| fileset::scan(root, &[]).expect("scan"))
    });
    group.finish();
}

fn fixture_bytes(root: &Path) -> u64 {
    let scanned = fileset::scan(root, &[]).expect("scan");
    scanned.values().map(|m| m.size).sum()
}

fn bench_pack(c: &mut Criterion, root: &Path, paths: &[String]) {
    let total = fixture_bytes(root);
    for (name, algo) in [
        ("gzip", CompressionAlgo::Gzip),
        ("zstd", CompressionAlgo::Zstd),
    ] {
        let mut group = c.benchmark_group("sync");
        group.throughput(Throughput::Bytes(total));
        group.bench_function(format!("pack_tar_{name}_2mib"), |b| {
            b.iter(|| fileset::pack_tar_with_algo(root, paths, algo).expect("pack"))
        });
        group.finish();
    }
}

fn bench_unpack(c: &mut Criterion, archives: &[(CompressionAlgo, Vec<u8>)]) {
    for (algo, archive) in archives {
        let name = match algo {
            CompressionAlgo::Gzip => "gzip",
            CompressionAlgo::Zstd => "zstd",
            CompressionAlgo::None => "none",
        };
        let mut group = c.benchmark_group("sync");
        group.throughput(Throughput::Bytes(archive.len() as u64));
        let mut seq: usize = 0;
        group.bench_function(format!("unpack_tar_{name}_2mib"), |b| {
            let dest_root = TempDir::new().expect("dest root");
            b.iter_batched(
                || {
                    let dir = dest_root.path().join(format!("it{seq}"));
                    seq += 1;
                    fs::create_dir_all(&dir).unwrap();
                    dir
                },
                |dir| fileset::unpack_tar(&dir, archive).expect("unpack"),
                BatchSize::LargeInput,
            );
        });
        group.finish();
    }
}

fn main() {
    let fixture = TempDir::new().expect("fixture dir");
    build_fixture(fixture.path());
    let paths = fixture_paths(fixture.path());

    // Warm one archive per algo for the unpack benches.
    let archives = vec![
        (
            CompressionAlgo::Gzip,
            fileset::pack_tar_with_algo(fixture.path(), &paths, CompressionAlgo::Gzip).unwrap(),
        ),
        (
            CompressionAlgo::Zstd,
            fileset::pack_tar_with_algo(fixture.path(), &paths, CompressionAlgo::Zstd).unwrap(),
        ),
    ];

    let mut crit = Criterion::default();
    bench_scan_hash(&mut crit, fixture.path());
    bench_pack(&mut crit, fixture.path(), &paths);
    bench_unpack(&mut crit, &archives);

    let _ = fs::remove_dir_all(fixture.path());
    crit.final_summary();
}
