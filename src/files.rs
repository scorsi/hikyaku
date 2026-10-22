//! Declared log files (ADR 0003, macOS): tailed by inode and offset, both saved in the spool
//! state. A file that shrinks or changes inode is read again from 0 (`file-truncated`).

use crate::logschema::MAX_LINE_BYTES;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

pub struct Tail {
    pub path: PathBuf,
    pub inode: u64,
    pub offset: u64,
}

pub struct Polled {
    /// Complete lines, without their newline.
    pub lines: Vec<Vec<u8>>,
    pub truncated: bool,
}

impl Tail {
    /// Reads up to `limit` complete lines appended since the last poll; a partial last line
    /// waits for its newline. The position moves only when the whole poll succeeded, so an I/O
    /// error mid-way reads the same lines again at the next poll.
    pub fn poll(&mut self, limit: usize) -> io::Result<Polled> {
        let mut out = Polled {
            lines: Vec::new(),
            truncated: false,
        };
        // The path is looked at without following a link, then the bytes and the size come
        // from the descriptor itself: a symlink planted in place of the log is refused, and a
        // rotation between the two calls (other inode) is retried at the next poll.
        let link = match fs::symlink_metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        if !link.file_type().is_file() {
            return Err(io::Error::other(
                "not a regular file (symlinks are refused)",
            ));
        }
        let f = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        let meta = f.metadata()?;
        if (meta.dev(), meta.ino()) != (link.dev(), link.ino()) {
            return Ok(out);
        }
        let mut offset = self.offset;
        if (self.inode != 0 && meta.ino() != self.inode) || meta.len() < offset {
            out.truncated = true;
            offset = 0;
        }
        let mut reader = BufReader::new(f);
        reader.seek(SeekFrom::Start(offset))?;
        let (lines, read) = read_lines(&mut reader, limit)?;
        out.lines = lines;
        (self.inode, self.offset) = (meta.ino(), offset + read);
        Ok(out)
    }
}

/// Bytes a poll reads at most (ADR 0002's memory budget, `MemoryMax=64M`).
const POLL_BYTES: u64 = 4 << 20;

/// Up to `limit` complete lines (without their newline, each cut to one byte over the schema's
/// line limit) and the bytes they took, at most about `POLL_BYTES`; an error drops them all,
/// so that the caller's position stays where it was.
fn read_lines(reader: &mut impl BufRead, limit: usize) -> io::Result<(Vec<Vec<u8>>, u64)> {
    let (mut lines, mut read) = (Vec::new(), 0);
    while lines.len() < limit && read < POLL_BYTES {
        let Some((line, n)) = read_line_bounded(reader, MAX_LINE_BYTES + 1)? else {
            break;
        };
        read += n;
        lines.push(line);
    }
    Ok((lines, read))
}

/// One complete line without its newline, and the bytes it took. At most `keep` bytes are
/// kept: the rest of a longer line is read and dropped, never held in memory. `None` when the
/// input ends before a newline.
pub fn read_line_bounded(r: &mut impl BufRead, keep: usize) -> io::Result<Option<(Vec<u8>, u64)>> {
    let (mut line, mut used) = (Vec::new(), 0u64);
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok(None);
        }
        let newline = buf.iter().position(|&c| c == b'\n');
        let chunk = &buf[..newline.unwrap_or(buf.len())];
        let room = keep.saturating_sub(line.len()).min(chunk.len());
        line.extend_from_slice(&chunk[..room]);
        let n = newline.map_or(buf.len(), |i| i + 1);
        r.consume(n);
        used += n as u64;
        if newline.is_some() {
            return Ok(Some((line, used)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tails_truncation_and_rotation() {
        let dir = std::env::temp_dir().join(format!("hikyaku-tail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("svc.log");
        let mut tail = Tail {
            path: path.clone(),
            inode: 0,
            offset: 0,
        };
        assert!(tail.poll(1000).unwrap().lines.is_empty()); // absent file

        fs::write(&path, b"one\ntwo\npart").unwrap();
        let p = tail.poll(1000).unwrap();
        assert_eq!(p.lines, [b"one".to_vec(), b"two".to_vec()]);
        assert!(!p.truncated);
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"ial\n").unwrap();
        assert_eq!(tail.poll(1000).unwrap().lines, [b"partial".to_vec()]);

        // Truncation: smaller than the offset, read from 0.
        fs::write(&path, b"new\n").unwrap();
        let p = tail.poll(1000).unwrap();
        assert!(p.truncated);
        assert_eq!(p.lines, [b"new".to_vec()]);

        // Rotation: same size or larger, another inode.
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"rotated-longer\n").unwrap();
        let p = tail.poll(1000).unwrap();
        assert!(p.truncated);
        assert_eq!(p.lines, [b"rotated-longer".to_vec()]);
        assert_eq!(tail.offset, 15);

        // A symlink planted in place of the log: refused, nothing read, position kept.
        let secret = dir.join("secret");
        fs::write(&secret, b"root only\n").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&secret, &path).unwrap();
        assert!(tail.poll(1000).is_err());
        assert_eq!(tail.offset, 15);
    }

    /// A reader that fails after its bytes, as a disk returning EIO mid-poll.
    struct FailAfter<'a>(&'a [u8]);
    impl io::Read for FailAfter<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                return Err(io::Error::other("EIO"));
            }
            let n = self.0.len().min(buf.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn a_read_error_keeps_the_lines_for_the_next_poll() {
        let mut ok = BufReader::new(&b"one\ntwo\npart"[..]);
        assert_eq!(read_lines(&mut ok, 10).unwrap().1, 8);
        // One complete line read, then an error: nothing is returned, so the tail's offset is
        // not moved and the line is read again.
        let mut failing = BufReader::with_capacity(4, FailAfter(b"one\ntwo"));
        assert!(read_lines(&mut failing, 10).is_err());
    }

    #[test]
    fn long_lines_are_cut_while_read() {
        let mut long = vec![b'x'; 3 * MAX_LINE_BYTES];
        long.extend_from_slice(b"\nnext\nno-newline");
        let mut r = BufReader::with_capacity(4096, &long[..]);
        let (lines, read) = read_lines(&mut r, 10).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].len(),
            MAX_LINE_BYTES + 1,
            "kept up to the limit only"
        );
        assert_eq!(lines[1], b"next");
        assert_eq!(
            read,
            3 * MAX_LINE_BYTES as u64 + 6,
            "the whole line is consumed"
        );
    }
}
