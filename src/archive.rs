//! Streaming decoders for AirDrop upload bodies.
//!
//! Finder uploads `application/x-dvzip`: frames of `[u32 BE header][payload]`, where the
//! header's top bit marks a stored (uncompressed) payload, otherwise zlib, and the low
//! 31 bits are the payload length. The decoded bytes are a cpio archive in odc format
//! (`070707` magic). Older senders upload that cpio directly as `application/x-cpio`.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Largest dvzip frame we accept; Finder uses 128 KiB.
const MAX_FRAME: usize = 16 << 20;

/// Decode one dvzip frame header. Returns (payload length, stored uncompressed).
pub fn dvzip_header(hdr: [u8; 4]) -> Result<(usize, bool)> {
    let v = u32::from_be_bytes(hdr);
    let len = (v & 0x7FFF_FFFF) as usize;
    if len > MAX_FRAME {
        bail!("dvzip frame too large: {len}");
    }
    Ok((len, v & 0x8000_0000 != 0))
}

pub fn dvzip_payload(payload: &[u8], stored: bool) -> Result<Vec<u8>> {
    if stored {
        return Ok(payload.to_vec());
    }
    let mut out = Vec::with_capacity(payload.len() * 2);
    flate2::read::ZlibDecoder::new(payload)
        .read_to_end(&mut out)
        .context("bad zlib frame")?;
    Ok(out)
}

const HEADER_LEN: usize = 76;
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

enum State {
    Header(Vec<u8>),
    Name { buf: Vec<u8>, size: usize, mode: u32, data: u64 },
    Data { left: u64, file: Option<File> },
    Done,
}

/// Push-based cpio (odc) extractor that writes regular files under `root`.
pub struct CpioExtractor {
    root: PathBuf,
    state: State,
    files: Vec<PathBuf>,
}

impl CpioExtractor {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf(), state: State::Header(Vec::new()), files: Vec::new() }
    }

    pub fn feed(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            match &mut self.state {
                State::Done => return Ok(()), // ignore padding after the trailer
                State::Header(buf) => {
                    let take = (HEADER_LEN - buf.len()).min(data.len());
                    buf.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    if buf.len() == HEADER_LEN {
                        let h = std::mem::take(buf);
                        if &h[..6] != b"070707" {
                            bail!("not a cpio odc header");
                        }
                        self.state = State::Name {
                            buf: Vec::new(),
                            mode: octal(&h[18..24])? as u32,
                            size: octal(&h[59..65])? as usize,
                            data: octal(&h[65..76])?,
                        };
                    }
                }
                State::Name { buf, size, mode, data: len } => {
                    let take = (*size - buf.len()).min(data.len());
                    buf.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    if buf.len() == *size {
                        let (name, mode, len) = (std::mem::take(buf), *mode, *len);
                        self.start_entry(&name, mode, len)?;
                    }
                }
                State::Data { left, file } => {
                    let take = (*left).min(data.len() as u64) as usize;
                    if let Some(f) = file {
                        f.write_all(&data[..take])?;
                    }
                    *left -= take as u64;
                    data = &data[take..];
                    if *left == 0 {
                        self.state = State::Header(Vec::new());
                    }
                }
            }
        }
        Ok(())
    }

    fn start_entry(&mut self, raw_name: &[u8], mode: u32, len: u64) -> Result<()> {
        let name = std::str::from_utf8(raw_name.strip_suffix(&[0]).unwrap_or(raw_name))
            .context("non-UTF-8 file name")?;
        if name == "TRAILER!!!" {
            self.state = State::Done;
            return Ok(());
        }
        let rel = safe_relative(name)?;
        let path = self.root.join(&rel);
        let mut file = None;
        match mode & S_IFMT {
            S_IFDIR => fs::create_dir_all(&path)?,
            S_IFREG if rel.as_os_str().is_empty() => bail!("file entry without a name"),
            // AppleDouble metadata (`._name`) that Finder packs next to each file.
            S_IFREG if rel.file_name().is_some_and(|n| n.as_encoded_bytes().starts_with(b"._")) => {}
            S_IFREG => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                file = Some(File::create(&path)?);
                self.files.push(path);
            }
            _ => {} // symlinks and specials are skipped
        }
        self.state = if len == 0 { State::Header(Vec::new()) } else { State::Data { left: len, file } };
        Ok(())
    }

    /// Regular files written, in archive order. Errors if the archive was cut short.
    pub fn finish(self) -> Result<Vec<PathBuf>> {
        match self.state {
            State::Done if !self.files.is_empty() => Ok(self.files),
            State::Done => bail!("archive contains no files"),
            _ => bail!("truncated cpio archive"),
        }
    }
}

fn octal(field: &[u8]) -> Result<u64> {
    let s = std::str::from_utf8(field)?;
    u64::from_str_radix(s, 8).with_context(|| format!("bad octal field {s:?}"))
}

/// Reject absolute paths and `..` so a sender can't write outside the transfer folder.
fn safe_relative(name: &str) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in Path::new(name).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => bail!("unsafe path in archive: {name:?}"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, mode: u32, data: &[u8]) -> Vec<u8> {
        let name = format!("{name}\0");
        let mut v = format!(
            "070707{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:011o}{:06o}{:011o}",
            0, 0, mode, 501, 20, 1, 0, 0, name.len(), data.len()
        )
        .into_bytes();
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(data);
        v
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dropbridge-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn extracts_files_fed_in_tiny_pieces() {
        let mut ar = entry(".", 0o040755, b"");
        ar.extend(entry("./dir/a.txt", 0o100644, b"hello"));
        ar.extend(entry("./empty", 0o100644, b""));
        ar.extend(entry("./._empty", 0o100644, b"\0\x05\x16\x07appledouble"));
        ar.extend(entry("TRAILER!!!", 0, b""));
        let root = tmp("ok");
        let mut x = CpioExtractor::new(&root);
        for b in ar.chunks(7) {
            x.feed(b).unwrap();
        }
        let files = x.finish().unwrap();
        assert_eq!(files, vec![root.join("dir/a.txt"), root.join("empty")]);
        assert_eq!(fs::read(root.join("dir/a.txt")).unwrap(), b"hello");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_path_traversal() {
        let root = tmp("evil");
        let mut x = CpioExtractor::new(&root);
        assert!(x.feed(&entry("../evil", 0o100644, b"x")).is_err());
        let mut x = CpioExtractor::new(&root);
        assert!(x.feed(&entry("/etc/evil", 0o100644, b"x")).is_err());
    }

    #[test]
    fn truncated_archive_is_an_error() {
        let root = tmp("trunc");
        let mut x = CpioExtractor::new(&root);
        let ar = entry("./a", 0o100644, b"hello");
        x.feed(&ar[..ar.len() - 2]).unwrap();
        assert!(x.finish().is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dvzip_frames() {
        assert_eq!(dvzip_header(0x8002_0000u32.to_be_bytes()).unwrap(), (0x20000, true));
        assert_eq!(dvzip_header(44u32.to_be_bytes()).unwrap(), (44, false));
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(b"070707abc").unwrap();
        assert_eq!(dvzip_payload(&z.finish().unwrap(), false).unwrap(), b"070707abc");
    }
}
