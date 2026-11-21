// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Ahmet Dedeler

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use tempfile::TempDir;

pub struct PendingClip {
    rx: mpsc::Receiver<io::Result<TempDir>>,
    cancel: Arc<AtomicBool>,
}

impl PendingClip {
    pub fn prepare(path: PathBuf, offset: u64, size: u64) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        std::thread::Builder::new().name("bk2-prepare".into()).spawn(move || {
            let _ = tx.send(copy_clip(&path, offset, size, &worker_cancel));
        })?;
        Ok(Self { rx, cancel })
    }

    pub fn poll(&self) -> Option<Result<TempDir, String>> {
        Some(match self.rx.try_recv() {
            Ok(result) => result.map_err(|err| format!("Read video: {err}")),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("Video preparation stopped".into()),
        })
    }
}

impl Drop for PendingClip {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(any(windows, test))]
pub fn first_audio_track(path: &Path) -> Option<u32> {
    let mut file = File::open(path).ok()?;
    let mut header = [0; 44];
    file.read_exact(&mut header).ok()?;
    if &header[..3] != b"KB2" { return None; }
    let tracks = u32::from_le_bytes(header[40..44].try_into().ok()?);
    if tracks == 0 || tracks > 256 { return None; }
    let table = if matches!(header[3], b'i' | b'j' | b'k' | b'n') { 48 } else { 44 };
    // Track IDs follow two u32 tables (maximum decoded bytes and audio format).
    file.seek(SeekFrom::Start(table + u64::from(tracks) * 8)).ok()?;
    let mut id = [0; 4];
    file.read_exact(&mut id).ok()?;
    Some(u32::from_le_bytes(id))
}

pub fn copy_clip(path: &Path, offset: u64, size: u64, cancel: &AtomicBool) -> io::Result<TempDir> {
    let mut archive = File::open(path)?;
    archive.seek(SeekFrom::Start(offset))?;
    let dir = tempfile::Builder::new().prefix("6stars-video-").tempdir()?;
    // a standalone .bk2: link it instead of copying (multi-GB cutscenes), falling back to a
    // copy when the temp folder is on another drive. The fixed name keeps odd paths safe.
    if offset == 0 && std::fs::metadata(path)?.len() == size && std::fs::hard_link(path, dir.path().join("clip.bk2")).is_ok() {
        return Ok(dir);
    }
    let mut clip = File::create(dir.path().join("clip.bk2"))?;
    let mut buf = vec![0; size.min(1024 * 1024) as usize];
    let mut remaining = size;
    while remaining > 0 {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "preview cancelled"));
        }
        let count = remaining.min(buf.len() as u64) as usize;
        archive.read_exact(&mut buf[..count])?;
        clip.write_all(&buf[..count])?;
        remaining -= count as u64;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_preview_copies_only_the_archive_entry() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"prefixKB2nvideoNEXT").unwrap();
        let cancel = AtomicBool::new(false);
        let dir = copy_clip(file.path(), 6, 9, &cancel).unwrap();
        assert_eq!(std::fs::read(dir.path().join("clip.bk2")).unwrap(), b"KB2nvideo");
        assert_eq!(
            copy_clip(file.path(), 6, 100, &cancel).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn standalone_clip_is_linked_whole() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"KB2nwholefile").unwrap();
        let dir = copy_clip(file.path(), 0, 13, &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read(dir.path().join("clip.bk2")).unwrap(), b"KB2nwholefile");
    }

    #[test]
    fn reads_nonzero_bink_audio_track_id() {
        let mut header = [0; 72];
        header[..4].copy_from_slice(b"KB2n");
        header[40..44].copy_from_slice(&2u32.to_le_bytes());
        header[64..68].copy_from_slice(&42u32.to_le_bytes());
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&header).unwrap();
        assert_eq!(first_audio_track(file.path()), Some(42));
    }
}
