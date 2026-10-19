//! Declared log files (ADR 0003, macOS): tailed by inode and offset, both saved in the spool
//! state. A file that shrinks or changes inode is read again from 0 (`file-truncated`).

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
    /// waits for its newline.
    pub fn poll(&mut self, limit: usize) -> io::Result<Polled> {
        let mut out = Polled {
            lines: Vec::new(),
            truncated: false,
        };
        let meta = match fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        if (self.inode != 0 && meta.ino() != self.inode) || meta.len() < self.offset {
            out.truncated = true;
            self.offset = 0;
        }
        self.inode = meta.ino();
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(f);
        while out.lines.len() < limit {
            let mut line = Vec::new();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 || line.last() != Some(&b'\n') {
                break;
            }
            self.offset += n as u64;
            line.pop();
            out.lines.push(line);
        }
        Ok(out)
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
    }
}
