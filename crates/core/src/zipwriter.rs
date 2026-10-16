//! Minimal ZIP writer (stored entries). The `zip` crate is unavailable at the moment (all
//! releases yanked), and diagnostics archives are small text files, so this is plenty.

use std::io::{self, Write};

struct Entry {
    name: String,
    crc: u32,
    size: u32,
    offset: u32,
}

pub struct ZipWriter<W: Write> {
    w: W,
    pos: u64,
    entries: Vec<Entry>,
}

impl<W: Write> ZipWriter<W> {
    pub fn new(w: W) -> Self {
        Self {
            w,
            pos: 0,
            entries: Vec::new(),
        }
    }

    fn put(&mut self, data: &[u8]) -> io::Result<()> {
        self.w.write_all(data)?;
        self.pos += data.len() as u64;
        Ok(())
    }

    /// Adds one file (UTF-8 name, `/` separators). Entries are limited to < 4 GiB.
    pub fn add(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let size = u32::try_from(data.len()).map_err(|_| io::Error::other("entry too large"))?;
        let offset = u32::try_from(self.pos).map_err(|_| io::Error::other("archive too large"))?;
        let crc = crc32fast::hash(data);
        let name_bytes = name.as_bytes();
        let mut h = Vec::with_capacity(30 + name_bytes.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header
        h.extend_from_slice(&20u16.to_le_bytes()); // version needed
        h.extend_from_slice(&0x0800u16.to_le_bytes()); // flags: UTF-8 names
        h.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        h.extend_from_slice(&0u16.to_le_bytes()); // time
        h.extend_from_slice(&0x21u16.to_le_bytes()); // date 1980-01-01
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // extra
        h.extend_from_slice(name_bytes);
        self.put(&h)?;
        self.put(data)?;
        self.entries.push(Entry {
            name: name.to_string(),
            crc,
            size,
            offset,
        });
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        let cd_start = self.pos;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let nb = e.name.as_bytes();
            let mut h = Vec::with_capacity(46 + nb.len());
            h.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central directory header
            h.extend_from_slice(&20u16.to_le_bytes()); // version made by
            h.extend_from_slice(&20u16.to_le_bytes()); // version needed
            h.extend_from_slice(&0x0800u16.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&0x21u16.to_le_bytes());
            h.extend_from_slice(&e.crc.to_le_bytes());
            h.extend_from_slice(&e.size.to_le_bytes());
            h.extend_from_slice(&e.size.to_le_bytes());
            h.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            h.extend_from_slice(&[0u8; 8]); // extra, comment, disk, internal attrs
            h.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            h.extend_from_slice(&e.offset.to_le_bytes());
            h.extend_from_slice(nb);
            self.put(&h)?;
        }
        let cd_size = (self.pos - cd_start) as u32;
        let mut t = Vec::with_capacity(22);
        t.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        t.extend_from_slice(&[0u8; 4]); // disk numbers
        t.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        t.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        t.extend_from_slice(&cd_size.to_le_bytes());
        t.extend_from_slice(&(cd_start as u32).to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        self.put(&t)?;
        self.w.flush()?;
        Ok(self.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the archive back with a tiny independent parser (central directory walk).
    fn list(data: &[u8]) -> Vec<(String, Vec<u8>, u32)> {
        let eocd = data.len() - 22;
        assert_eq!(&data[eocd..eocd + 4], &0x0605_4b50u32.to_le_bytes());
        let n = u16::from_le_bytes([data[eocd + 10], data[eocd + 11]]) as usize;
        let mut off = u32::from_le_bytes(data[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        let mut out = Vec::new();
        for _ in 0..n {
            assert_eq!(&data[off..off + 4], &0x0201_4b50u32.to_le_bytes());
            let crc = u32::from_le_bytes(data[off + 16..off + 20].try_into().unwrap());
            let size = u32::from_le_bytes(data[off + 24..off + 28].try_into().unwrap()) as usize;
            let nl = u16::from_le_bytes([data[off + 28], data[off + 29]]) as usize;
            let lho = u32::from_le_bytes(data[off + 42..off + 46].try_into().unwrap()) as usize;
            let name = String::from_utf8(data[off + 46..off + 46 + nl].to_vec()).unwrap();
            let lnl = u16::from_le_bytes([data[lho + 26], data[lho + 27]]) as usize;
            let start = lho + 30 + lnl;
            out.push((name, data[start..start + size].to_vec(), crc));
            off += 46 + nl;
        }
        out
    }

    #[test]
    fn roundtrip() {
        let mut z = ZipWriter::new(Vec::new());
        z.add("a.txt", b"hello").unwrap();
        z.add("logs/ü.log", "Grüße".as_bytes()).unwrap();
        let bytes = z.finish().unwrap();
        let l = list(&bytes);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].0, "a.txt");
        assert_eq!(l[0].1, b"hello");
        assert_eq!(l[0].2, crc32fast::hash(b"hello"));
        assert_eq!(l[1].0, "logs/ü.log");
    }
}
