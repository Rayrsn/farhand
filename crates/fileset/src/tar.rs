use crate::scan::{get_file_mode, FilesetError};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::{self, File};
use std::path::Path;
use tar::{Archive, Builder, Header};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionAlgo {
    #[default]
    Zstd,
    Gzip,
    None,
}

impl CompressionAlgo {
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("zstd") => Self::Zstd,
            Some("gzip") | Some("gz") => Self::Gzip,
            Some("none") | Some("plain") | Some("tar") => Self::None,
            _ => Self::Zstd,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Zstd => "zstd",
            Self::Gzip => "gzip",
            Self::None => "none",
        }
    }
}

fn build_tar_entries<W: std::io::Write>(
    root: &Path,
    paths: &[String],
    writer: W,
) -> Result<W, FilesetError> {
    let mut builder = Builder::new(writer);

    for rel_path in paths {
        let rel_buf = match protocol::from_wire_path(rel_path) {
            Ok(p) => p,
            Err(_) => return Err(FilesetError::InsecurePath(rel_path.clone())),
        };

        let local_path = root.join(&rel_buf);
        let metadata = match fs::symlink_metadata(&local_path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(FilesetError::Io(e)),
        };

        let file_type = metadata.file_type();
        let header_wire_path = protocol::to_wire_path(&rel_buf);

        if file_type.is_file() {
            let mut file = File::open(&local_path)?;
            let mut header = Header::new_gnu();
            header.set_size(metadata.len());
            header.set_mode(get_file_mode(&metadata));
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();

            builder
                .append_data(&mut header, &header_wire_path, &mut file)
                .map_err(|e| FilesetError::Tar(e.to_string()))?;
        } else if file_type.is_dir() {
            let mut header = Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_cksum();

            let header_path = format!("{}/", header_wire_path);
            let empty = &mut std::io::empty();
            builder
                .append_data(&mut header, header_path, empty)
                .map_err(|e| FilesetError::Tar(e.to_string()))?;
        } else if file_type.is_symlink() {
            if let Ok(target) = fs::read_link(&local_path) {
                let mut header = Header::new_gnu();
                header.set_size(0);
                header.set_entry_type(tar::EntryType::Symlink);
                let target_str = protocol::to_wire_path(&target);
                header
                    .set_link_name(&target_str)
                    .map_err(|e| FilesetError::Tar(e.to_string()))?;
                header.set_cksum();

                let empty = &mut std::io::empty();
                builder
                    .append_data(&mut header, &header_wire_path, empty)
                    .map_err(|e| FilesetError::Tar(e.to_string()))?;
            }
        }
    }

    builder
        .into_inner()
        .map_err(|e| FilesetError::Tar(e.to_string()))
}

/// Pack an arbitrary list of relative file paths from `root` into a compressed tar archive.
pub fn pack_tar_with_algo(
    root: &Path,
    paths: &[String],
    algo: CompressionAlgo,
) -> Result<Vec<u8>, FilesetError> {
    match algo {
        CompressionAlgo::Zstd => {
            let encoder = zstd::stream::write::Encoder::new(Vec::new(), 3)?;
            let finished = build_tar_entries(root, paths, encoder)?;
            finished.finish().map_err(FilesetError::Io)
        }
        CompressionAlgo::Gzip => {
            let encoder = GzEncoder::new(Vec::new(), Compression::default());
            let finished = build_tar_entries(root, paths, encoder)?;
            finished.finish().map_err(FilesetError::Io)
        }
        CompressionAlgo::None => build_tar_entries(root, paths, Vec::new()),
    }
}

/// Pack an arbitrary list of relative file paths from `root` using default compression (Zstandard).
pub fn pack_tar(root: &Path, paths: &[String]) -> Result<Vec<u8>, FilesetError> {
    pack_tar_with_algo(root, paths, CompressionAlgo::Zstd)
}

/// Unpack a compressed or plain tar archive into `dest_dir` with strict Zip-Slip path sanitization.
/// Automatically detects compression format (Zstandard, Gzip, or uncompressed) via magic bytes.
pub fn unpack_tar(dest_dir: &Path, data: &[u8]) -> Result<(), FilesetError> {
    if data.is_empty() {
        return Ok(());
    }

    if data.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        let decoder = zstd::stream::read::Decoder::new(data)?;
        unpack_archive_reader(dest_dir, decoder)
    } else if data.starts_with(&[0x1F, 0x8B]) {
        let decoder = GzDecoder::new(data);
        unpack_archive_reader(dest_dir, decoder)
    } else {
        unpack_archive_reader(dest_dir, data)
    }
}

fn unpack_archive_reader<R: std::io::Read>(dest_dir: &Path, reader: R) -> Result<(), FilesetError> {
    fs::create_dir_all(dest_dir)?;
    let canonical_dest = dest_dir.canonicalize()?;
    let mut archive = Archive::new(reader);

    for entry_res in archive
        .entries()
        .map_err(|e| FilesetError::Tar(e.to_string()))?
    {
        let mut entry = entry_res.map_err(|e| FilesetError::Tar(e.to_string()))?;
        let raw_bytes = entry.path_bytes();
        let path_str = std::str::from_utf8(&raw_bytes)
            .map_err(|_| FilesetError::InsecurePath("invalid utf-8 in tar path".to_string()))?;

        // Prevent Zip-Slip: reject absolute paths, backslashes, or directory traversal sequences
        let path_clean = path_str.trim_end_matches('/');
        let safe_rel = match protocol::from_wire_path(path_clean) {
            Ok(p) => p,
            Err(_) => return Err(FilesetError::InsecurePath(path_str.to_string())),
        };

        let target_path = canonical_dest.join(&safe_rel);

        // Security check: ensure target path stays within destination root
        let parent = target_path.parent().unwrap_or(&canonical_dest);
        fs::create_dir_all(parent)?;

        let canonical_parent = parent.canonicalize()?;
        if !canonical_parent.starts_with(&canonical_dest) {
            return Err(FilesetError::EscapesTargetRoot(path_str.to_owned()));
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
        fs::write(
            src_dir.path().join("sub/nested/file.txt"),
            b"nested content",
        )
        .unwrap();
        fs::write(src_dir.path().join("root.txt"), b"root content").unwrap();

        let paths = vec!["sub/nested/file.txt".to_string(), "root.txt".to_string()];

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
        assert!(!dst_dir
            .path()
            .parent()
            .unwrap()
            .join("escaped.txt")
            .exists());
    }

    #[test]
    fn test_reject_backslash_in_tar_header() {
        use std::io::Write;
        let dst_dir = tempdir().unwrap();

        let mut gz_encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut header = [0u8; 512];
            let win_name = b"sub\\dir\\file.txt";
            header[..win_name.len()].copy_from_slice(win_name);
            header[100..108].copy_from_slice(b"0000644\0");
            header[124..136].copy_from_slice(b"00000000004\0");
            header[156] = b'0';
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");

            header[148..156].copy_from_slice(b"        ");
            let sum: u32 = header.iter().map(|&b| b as u32).sum();
            let cksum_str = format!("{:06o}\0 ", sum);
            header[148..156].copy_from_slice(cksum_str.as_bytes());

            gz_encoder.write_all(&header).unwrap();
            let mut payload = [0u8; 512];
            payload[..4].copy_from_slice(b"test");
            gz_encoder.write_all(&payload).unwrap();
            gz_encoder.write_all(&[0u8; 1024]).unwrap();
        }
        let payload = gz_encoder.finish().unwrap();

        let err = unpack_tar(dst_dir.path(), &payload).unwrap_err();
        match err {
            FilesetError::InsecurePath(p) => assert!(p.contains('\\')),
            _ => panic!("expected InsecurePath, got {:?}", err),
        }
    }

    #[test]
    fn test_pack_and_unpack_all_compression_algos() {
        let src = tempdir().unwrap();
        fs::write(src.path().join("hello.txt"), b"farhand zstd test").unwrap();
        let paths = vec!["hello.txt".to_string()];

        for algo in [
            CompressionAlgo::Zstd,
            CompressionAlgo::Gzip,
            CompressionAlgo::None,
        ] {
            let packed = pack_tar_with_algo(src.path(), &paths, algo).unwrap();
            assert!(!packed.is_empty());

            if algo == CompressionAlgo::Zstd {
                assert!(packed.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]));
            } else if algo == CompressionAlgo::Gzip {
                assert!(packed.starts_with(&[0x1F, 0x8B]));
            }

            let dst = tempdir().unwrap();
            unpack_tar(dst.path(), &packed).unwrap();
            assert_eq!(
                fs::read(dst.path().join("hello.txt")).unwrap(),
                b"farhand zstd test"
            );
        }
    }
}
