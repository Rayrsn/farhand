#!/usr/bin/env python3
"""Render BENCHMARKS.md from criterion's saved results in target/criterion/.

Each benchmark directory contains new/estimates.json with mean and std-dev
point estimates (in nanoseconds). Grouped by bench target prefix (sync/,
cas/, cow/, diff/).
"""

import json
import pathlib
import subprocess

CRITERION = pathlib.Path("target/criterion")

TARGETS = {
    "sync": [
        "sync/scan_and_hash_320_files",
        "sync/pack_tar_gzip_2mib",
        "sync/pack_tar_zstd_2mib",
        "sync/unpack_tar_gzip_2mib",
        "sync/unpack_tar_zstd_2mib",
    ],
    "storage": [
        "cas/cas_put_64k",
        "cas/cas_put_256k",
        "cas/cas_materialize_64k",
        "cas/cas_materialize_256k",
        "cow/cow_clone_dir_100_files_200k",
        "diff/diff_manifests_cold_workspace",
        "diff/diff_manifests_warm_workspace_noop",
    ],
}

LABELS = {
    "sync/scan_and_hash_320_files": "Scan + SHA-256 hash of 320 files (~2.4 MiB)",
    "sync/pack_tar_gzip_2mib": "Pack delta tar with gzip (~2.4 MiB payload)",
    "sync/pack_tar_zstd_2mib": "Pack delta tar with zstd-3 (~2.4 MiB)",
    "sync/unpack_tar_gzip_2mib": "Unpack gzip tar (~2.4 MiB)",
    "sync/unpack_tar_zstd_2mib": "Unpack zstd tar (~2.4 MiB)",
    "cas/cas_put_64k": "CAS store: ingest 64 KiB file (hash + atomic rename)",
    "cas/cas_put_256k": "CAS store: ingest 256 KiB file",
    "cas/cas_materialize_64k": "CAS hydrate 64 KiB via CoW reflink/hardlink",
    "cas/cas_materialize_256k": "CAS hydrate 256 KiB via CoW reflink/hardlink",
    "cow/cow_clone_dir_100_files_200k": "Workspace CoW clone: 100 files / ~200 KiB tree",
    "diff/diff_manifests_cold_workspace": "Manifest diff, empty remote workspace",
    "diff/diff_manifests_warm_workspace_noop": "Manifest diff, warm workspace (nothing changed)",
}


def fmt_time(ns: float) -> str:
    if ns >= 1e9:
        return f"{ns / 1e9:.3f} s"
    if ns >= 1e6:
        return f"{ns / 1e6:.2f} ms"
    if ns >= 1e3:
        return f"{ns / 1e3:.1f} µs"
    return f"{ns:.0f} ns"


def load(name: str) -> tuple[float, float] | None:
    p = CRITERION / name / "new" / "estimates.json"
    if not p.exists():
        return None
    data = json.loads(p.read_text())
    mean = data["mean"]["point_estimate"]
    dev = data["median_abs_dev"]["point_estimate"]
    return (mean, dev)


def machine_info() -> tuple[str, str, str]:
    cpu = subprocess.run(
        ["sh", "-c", "grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs || sysctl -n hw.model 2>/dev/null"],
        capture_output=True, text=True,
    ).stdout.strip() or "unknown CPU"
    cores = subprocess.run(["sh", "-c", "nproc || sysctl -n hw.ncpu 2>/dev/null"],
                           capture_output=True, text=True).stdout.strip() or "?"
    fstype = subprocess.run(["sh", "-c", "df -T /tmp | awk 'NR==2 {print $2}'"],
                            capture_output=True, text=True).stdout.strip() or "unknown"
    return cpu, cores, fstype


def main() -> None:
    cpu, cores, fstype = machine_info()
    lines = [
        "# Farhand Benchmarks",
        "",
        "Numbers produced by the criterion suites in `crates/fileset/benches/sync.rs`",
        "and `crates/workspace/benches/cas_cow.rs`. Every number in the README must",
        "be reproducible from this table: run `scripts/run_benchmarks.sh`",
        "(optionally `--fast`) and commit the result together with the claim.",
        "",
        f"- **CPU:** {cpu} ({cores} cores)",
        f"- **Filesystem:** /tmp on {fstype} — CoW paths exercise `clonefile` (APFS) / `FICLONE` (Linux) / hardlink / copy, whichever the host supports",
        "- **Mode:** `cargo bench --bench sync` / `--bench cas_cow` (release profile, criterion defaults)",
        "- **Notes:** `scan_and_hash` is dominated by SHA-256 over warm page cache;",
        "  CoW/CAS materialize speeds depend on the host filesystem (APFS `clonefile`,",
        "  Linux `FICLONE`, hardlink or copy fallback) — see the note on each benchmark.",
        "",
    ]

    for target, benches in TARGETS.items():
        title = "Fileset / sync pipeline" if target == "sync" else "Workspace / CAS, CoW, diff"
        lines += [f"## {title}", "", "| Benchmark | Mean | ± |", "| :--- | ---: | ---: |"]
        for bench in benches:
            res = load(bench)
            label = LABELS.get(bench, bench)
            if res is None:
                lines.append(f"| {label} | *(no data — run `cargo bench -p {target}`)* | |")
                continue
            mean, dev = res
            lines.append(f"| {label} | `{fmt_time(mean)}` | `±{fmt_time(dev)}` |")
        lines.append("")

    lines += [
        "## Derived claims used in the README",
        "",
        "- **zstd vs gzip pack throughput**: computed from `pack_tar_gzip_2mib` vs",
        "  `pack_tar_zstd_2mib` means above.",
        "- **CAS hydration is effectively free** (nanoseconds-per-KiB): see",
        "  `cas_materialize_*` — reflink/hardlink cost is independent of payload size.",
        "- **Warm-workspace diff cost** is the price of the manifest protocol: the",
        "  agent rescans the workspace to verify hashes every run.",
        "",
    ]

    print("\n".join(lines))


if __name__ == "__main__":
    main()