use crate::scan::{get_file_mode, FilesetError};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::{self, File};
use std::path::Path;
use tar::{Archive, Builder, Header};

/// Pack an arbitrary list of relative file paths from `root` into a gzip-compressed tar archive.
pub fn pack_tar(root: &Path, paths: &[String]) -> Result<Vec<u8>, FilesetError> {
    let mut gz_encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut gz_encoder);

        for rel_path in paths {
            let clean_rel = rel_path.trim_matches('/');
            if clean_rel.is_empty() {
                continue;
            }

            // Security check: reject paths attempting traversal
            if clean_rel.starts_with("..") || clean_rel.contains("/../") || clean_rel.contains("\\..\\") {
                return Err(FilesetError::InsecurePath(rel_path.clone()));
            }

            let local_path = root.join(clean_rel);
            let metadata = match fs::symlink_metadata(&local_path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(FilesetError::Io(e)),
            };

            let file_type = metadata.file_type();

            if file_type.is_file() {
                let mut file = File::open(&local_path)?;
                let mut header = Header::new_gnu();
                header.set_size(metadata.len());
                header.set_mode(get_file_mode(&metadata));
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();

                // Store wire-normalized forward-slash path in tar header
                let header_path = clean_rel.replace('\\', "/");
                builder
                    .append_data(&mut header, header_path, &mut file)
                    .map_err(|e| FilesetError::Tar(e.to_string()))?;
            } else if file_type.is_dir() {
                let mut header = Header::new_gnu();
                header.set_size(0);
                header.set_mode(0o755);
                header.set_entry_type(tar::EntryType::Directory);
                header.set_cksum();

                let header_path = format!("{}/", clean_rel.replace('\\', "/"));
                let empty = &mut std::io::empty();
                builder
                    .append_data(&mut header, header_path, empty)
                    .map_err(|e| FilesetError::Tar(e.to_string()))?;
            } else if file_type.is_symlink() {
                if let Ok(target) = fs::read_link(&local_path) {
                    let mut header = Header::new_gnu();
                    header.set_size(0);
                    header.set_entry_type(tar::EntryType::Symlink);
                    let target_str = target.to_string_lossy().replace('\\', "/");
                    header
                        .set_link_name(&target_str)
                        .map_err(|e| FilesetError::Tar(e.to_string()))?;
                    header.set_cksum();

                    let header_path = clean_rel.replace('\\', "/");
                    let empty = &mut std::io::empty();
                    builder
                        .append_data(&mut header, header_path, empty)
                        .map_err(|e| FilesetError::Tar(e.to_string()))?;
                }
            }
        }

        builder.finish().map_err(|e| FilesetError::Tar(e.to_string()))?;
    }

    gz_encoder.finish().map_err(FilesetError::Io)
}

/// Unpack a gzip-compressed tar archive into `dest_dir` with strict Zip-Slip path sanitization.
pub fn unpack_tar(dest_dir: &Path, data: &[u8]) -> Result<(), FilesetError> {
    if data.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(dest_dir)?;
    let canonical_dest = dest_dir.canonicalize()?;

    let gz_decoder = GzDecoder::new(data);
    let mut archive = Archive::new(gz_decoder);

    for entry_res in archive.entries().map_err(|e| FilesetError::Tar(e.to_string()))? {
        let mut entry = entry_res.map_err(|e| FilesetError::Tar(e.to_string()))?;
        let entry_path = entry.path().map_err(|e| FilesetError::Tar(e.to_string()))?;
        let path_str = entry_path.to_string_lossy();

        // Prevent Zip-Slip: reject absolute paths or directory traversal sequences
        if entry_path.is_absolute()
            || path_str.starts_with("..")
            || path_str.contains("/../")
            || path_str.contains("\\..\\")
        {
            return Err(FilesetError::InsecurePath(path_str.into_owned()));
        }

        let target_path = canonical_dest.join(&entry_path);

        // Security check: ensure target path stays within destination root
        let parent = target_path.parent().unwrap_or(&canonical_dest);
        fs::create_dir_all(parent)?;

        let canonical_parent = parent.canonicalize()?;
        if !canonical_parent.starts_with(&canonical_dest) {
            return Err(FilesetError::EscapesTargetRoot(path_str.into_owned()));
        }

        entry
            .unpack(&target_path)
            .map_err(|e| FilesetError::Tar(e.to_string()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_pack_and_unpack_roundtrip() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create sample source files
        fs::create_dir_all(src_dir.path().join("sub/nested")).unwrap();
        fs::write(src_dir.path().join("sub/nested/file.txt"), b"nested content").unwrap();
        fs::write(src_dir.path().join("root.txt"), b"root content").unwrap();

        let paths = vec![
            "sub/nested/file.txt".to_string(),
            "root.txt".to_string(),
        ];

        let tar_gz = pack_tar(src_dir.path(), &paths).unwrap();
        assert!(!tar_gz.is_empty());

        unpack_tar(dst_dir.path(), &tar_gz).unwrap();

        assert_eq!(
            fs::read(dst_dir.path().join("sub/nested/file.txt")).unwrap(),
            b"nested content"
        );
        assert_eq!(
            fs::read(dst_dir.path().join("root.txt")).unwrap(),
            b"root content"
        );
    }

    #[test]
    fn test_empty_tar_unpack() {
        let dst_dir = tempdir().unwrap();
        assert!(unpack_tar(dst_dir.path(), &[]).is_ok());
    }

    #[test]
    fn test_zip_slip_rejection() {
        use std::io::Write;
        let dst_dir = tempdir().unwrap();

        // Construct a malicious tar archive directly with traversal path in header
        let mut gz_encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut header = [0u8; 512];
            let evil_name = b"../../escaped.txt";
            header[..evil_name.len()].copy_from_slice(evil_name);
            header[100..108].copy_from_slice(b"0000644\0");
            header[124..136].copy_from_slice(b"00000000013\0"); // 11 bytes in octal
            header[156] = b'0';
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");

            header[148..156].copy_from_slice(b"        ");
            let sum: u32 = header.iter().map(|&b| b as u32).sum();
            let cksum_str = format!("{:06o}\0 ", sum);
            header[148..156].copy_from_slice(cksum_str.as_bytes());

            gz_encoder.write_all(&header).unwrap();
            let mut payload = [0u8; 512];
            payload[..11].copy_from_slice(b"hacked_data");
            gz_encoder.write_all(&payload).unwrap();
            gz_encoder.write_all(&[0u8; 1024]).unwrap();
        }
        let malicious_payload = gz_encoder.finish().unwrap();

        let err = unpack_tar(dst_dir.path(), &malicious_payload).unwrap_err();
        match err {
            FilesetError::InsecurePath(_) | FilesetError::EscapesTargetRoot(_) => {}
            _ => panic!("expected InsecurePath or EscapesTargetRoot, got {:?}", err),
        }

        // Verify escaped file was NOT written
        assert!(!dst_dir.path().parent().unwrap().join("escaped.txt").exists());
    }
}
