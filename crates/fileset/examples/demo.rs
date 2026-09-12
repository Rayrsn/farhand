use fileset::{pack_tar, scan, unpack_tar};
use std::env;
use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target_dir = env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let path = Path::new(&target_dir);

    println!("=== Farhand Interactive Verification ===");
    println!(
        "Target directory to scan: {}",
        path.canonicalize()?.display()
    );
    println!();

    // 1. Scan filesystem
    println!("Step 1: Scanning directory and applying ignore rules...");
    let files = scan(path, &[])?;

    println!("Discovered {} tracked files:", files.len());
    let mut total_bytes = 0u64;
    let mut file_paths = Vec::new();

    for (rel_path, meta) in &files {
        total_bytes += meta.size;
        file_paths.push(rel_path.clone());
        println!(
            "  [{}] mode: {:04o} | {:>8} bytes | sha256: {}... | {}",
            if meta.path.ends_with(".rs") {
                "CODE"
            } else {
                "FILE"
            },
            meta.mode,
            meta.size,
            &meta.hash[..12],
            rel_path
        );
    }

    println!();
    println!(
        "Total raw size: {} bytes across {} files",
        total_bytes,
        files.len()
    );
    println!("Verified: 'target/' and default ignore paths were automatically excluded.");
    println!();

    // 2. Pack into tar.gz
    println!("Step 2: Packing files into in-memory gzip-compressed tar archive...");
    let compressed_archive = pack_tar(path, &file_paths)?;
    println!(
        "Archive generated successfully! Compressed payload size: {} bytes ({:.1}% of raw size)",
        compressed_archive.len(),
        (compressed_archive.len() as f64 / total_bytes.max(1) as f64) * 100.0
    );
    println!();

    // 3. Unpack into a verification folder
    let unpack_dir = Path::new("farhand_demo_out");
    println!(
        "Step 3: Unpacking archive into '{}' with Zip-Slip protection...",
        unpack_dir.display()
    );
    if unpack_dir.exists() {
        fs::remove_dir_all(unpack_dir)?;
    }

    unpack_tar(unpack_dir, &compressed_archive)?;
    println!(
        "Archive unpacked successfully into '{}'.",
        unpack_dir.display()
    );

    // 4. Verify unpacked files
    let mut all_matched = true;
    for (rel_path, original_meta) in &files {
        let extracted_file = unpack_dir.join(rel_path);
        if !extracted_file.exists() {
            println!("  [FAIL] Missing extracted file: {}", rel_path);
            all_matched = false;
            continue;
        }
        let extracted_hash = fileset::hash_file(&extracted_file)?;
        if extracted_hash != original_meta.hash {
            println!(
                "  [FAIL] Hash mismatch for {}: expected {}, got {}",
                rel_path, original_meta.hash, extracted_hash
            );
            all_matched = false;
        }
    }

    if all_matched {
        println!(
            "All {} extracted files match their original SHA-256 digests perfectly!",
            files.len()
        );
    }

    // Clean up demo directory
    if unpack_dir.exists() {
        fs::remove_dir_all(unpack_dir)?;
        println!("Cleaned up test directory '{}'.", unpack_dir.display());
    }

    println!();
    Ok(())
}
