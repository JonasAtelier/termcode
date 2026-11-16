//! MD5 (RFC 1321), for the Explorer's file tooltip: in-process, so a hash nobody waits
//! for any more stops (`file`'s `stop`) instead of running to its end.

use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const SHIFT: [u32; 64] = {
    let r = [[7, 12, 17, 22], [5, 9, 14, 20], [4, 11, 16, 23], [6, 10, 15, 21]];
    let mut s = [0; 64];
    let mut i = 0;
    while i < 64 {
        s[i] = r[i / 16][i % 4];
        i += 1;
    }
    s
};

pub struct Md5 {
    state: [u32; 4],
    k: [u32; 64],
    buf: [u8; 64],
    fill: usize,
    total: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        // K[i] = floor(|sin(i + 1)| * 2^32), as the RFC defines it.
        let k = std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32);
        Md5 { state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476], k, buf: [0; 64], fill: 0, total: 0 }
    }
}

impl Md5 {
    fn block(&mut self, b: &[u8; 64]) {
        let m: [u32; 16] = std::array::from_fn(|i| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]));
        let [mut a, mut bb, mut c, mut d] = self.state;
        for (i, (&k, &shift)) in self.k.iter().zip(&SHIFT).enumerate() {
            let (f, g) = match i / 16 {
                0 => ((bb & c) | (!bb & d), i),
                1 => ((d & bb) | (!d & c), (5 * i + 1) % 16),
                2 => (bb ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (bb | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k).wrapping_add(m[g]);
            (a, d, c) = (d, c, bb);
            bb = bb.wrapping_add(f.rotate_left(shift));
        }
        for (s, v) in self.state.iter_mut().zip([a, bb, c, d]) {
            *s = s.wrapping_add(v);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let n = (64 - self.fill).min(data.len());
            self.buf[self.fill..self.fill + n].copy_from_slice(&data[..n]);
            (self.fill, data) = (self.fill + n, &data[n..]);
            if self.fill == 64 {
                let b = self.buf;
                self.block(&b);
                self.fill = 0;
            }
        }
    }

    /// The digest as 32 lowercase hex digits.
    pub fn hex(mut self) -> String {
        let bits = self.total.wrapping_mul(8);
        let pad = if self.fill < 56 { 56 - self.fill } else { 120 - self.fill };
        let mut tail = vec![0u8; pad];
        tail[0] = 0x80;
        self.update(&tail);
        self.update(&bits.to_le_bytes());
        self.state.iter().flat_map(|s| s.to_le_bytes()).map(|b| format!("{b:02x}")).collect()
    }
}

/// The MD5 of the file at `path`; None once `stop` is set (checked every 1 MB).
pub fn file(path: &Path, stop: &AtomicBool) -> io::Result<Option<String>> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Md5::default();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match f.read(&mut buf) {
            Ok(0) => return Ok(Some(h.hex())),
            Ok(n) => h.update(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_1321_test_suite() {
        let md5 = |s: &str| {
            let mut h = Md5::default();
            h.update(s.as_bytes());
            h.hex()
        };
        assert_eq!(md5(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5("a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(md5("message digest"), "f96b697d7cb7938d525a2f31aaf161d0");
        assert_eq!(md5("abcdefghijklmnopqrstuvwxyz"), "c3fcd3d76192e4007dfb496cca67e13b");
        assert_eq!(md5("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"), "d174ab98d277d9f5a5611c2c9f419d9f");
        assert_eq!(md5(&"1234567890".repeat(8)), "57edf4a22be3c955ac49da2e2107b67a");
        // Fed in pieces across block edges: the same digest.
        let mut h = Md5::default();
        for part in "1234567890".repeat(8).as_bytes().chunks(7) {
            h.update(part);
        }
        assert_eq!(h.hex(), "57edf4a22be3c955ac49da2e2107b67a");
    }

    #[test]
    fn a_file_hashes_and_a_stop_ends_it() {
        let p = std::env::temp_dir().join(format!("termcode-md5-{}", std::process::id()));
        std::fs::write(&p, "x\n").unwrap();
        assert_eq!(file(&p, &AtomicBool::new(false)).unwrap().as_deref(), Some("401b30e3b8b5d629635a5c613cdb7919"));
        assert_eq!(file(&p, &AtomicBool::new(true)).unwrap(), None);
        std::fs::remove_file(&p).unwrap();
    }
}
