//! Shared helpers for the Phase 0 test binaries.
//!
//! Raw recording format (`out/recording.raw`): little-endian `u32` width and
//! height, then per frame a `u64` timestamp in nanoseconds followed by
//! `width * height * 4` bytes of RGBA.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

pub fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../out");
    fs::create_dir_all(&dir).expect("create out dir");
    dir.canonicalize().expect("canonical out dir")
}

pub struct RawWriter {
    file: BufWriter<File>,
    frame_len: usize,
}

impl RawWriter {
    pub fn create(path: &PathBuf, width: u32, height: u32) -> io::Result<Self> {
        let mut file = BufWriter::with_capacity(8 << 20, File::create(path)?);
        file.write_all(&width.to_le_bytes())?;
        file.write_all(&height.to_le_bytes())?;
        Ok(Self { file, frame_len: (width * height * 4) as usize })
    }

    pub fn push(&mut self, pts_ns: u64, rgba: &[u8]) -> io::Result<()> {
        assert_eq!(rgba.len(), self.frame_len, "frame size mismatch");
        self.file.write_all(&pts_ns.to_le_bytes())?;
        self.file.write_all(rgba)
    }

    pub fn finish(mut self) -> io::Result<()> {
        self.file.flush()
    }
}

pub struct RawReader {
    file: BufReader<File>,
    pub width: u32,
    pub height: u32,
}

impl RawReader {
    pub fn open(path: &PathBuf) -> io::Result<Self> {
        let mut file = BufReader::new(File::open(path)?);
        let mut b = [0u8; 4];
        file.read_exact(&mut b)?;
        let width = u32::from_le_bytes(b);
        file.read_exact(&mut b)?;
        let height = u32::from_le_bytes(b);
        Ok(Self { file, width, height })
    }

    /// Next frame as (timestamp in ns, RGBA bytes), or `None` at end of file.
    pub fn next_frame(&mut self) -> io::Result<Option<(u64, Vec<u8>)>> {
        let mut ts = [0u8; 8];
        match self.file.read_exact(&mut ts) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let mut px = vec![0u8; (self.width * self.height * 4) as usize];
        self.file.read_exact(&mut px)?;
        Ok(Some((u64::from_le_bytes(ts), px)))
    }
}
