//! Minimal WAV reader / writer (PCM 8-32 bit, IEEE float 32/64, extensible header).

use std::io::{BufWriter, Read, Seek, SeekFrom, Write};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Format {
    Pcm16,
    Pcm24,
    Float32,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Pcm24, Format::Float32, Format::Pcm16];
    pub fn name(self) -> &'static str {
        match self {
            Format::Pcm16 => "16-bit PCM",
            Format::Pcm24 => "24-bit PCM",
            Format::Float32 => "32-bit float",
        }
    }
    pub fn bytes(self) -> usize {
        match self {
            Format::Pcm16 => 2,
            Format::Pcm24 => 3,
            Format::Float32 => 4,
        }
    }
}

pub struct WavWriter {
    f: BufWriter<std::fs::File>,
    fmt: Format,
    ch: u16,
    sr: u32,
    pub frames: u64,
}

impl WavWriter {
    pub fn create(path: &std::path::Path, fmt: Format, ch: u16, sr: u32) -> std::io::Result<Self> {
        let mut w = Self { f: BufWriter::with_capacity(1 << 18, std::fs::File::create(path)?), fmt, ch, sr, frames: 0 };
        w.header()?;
        Ok(w)
    }

    fn header(&mut self) -> std::io::Result<()> {
        let bytes = self.fmt.bytes() as u32;
        let data = (self.frames * self.ch as u64 * bytes as u64).min(u32::MAX as u64 - 64) as u32;
        let f = &mut self.f;
        f.write_all(b"RIFF")?;
        f.write_all(&(36 + data).to_le_bytes())?;
        f.write_all(b"WAVEfmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&(if self.fmt == Format::Float32 { 3u16 } else { 1u16 }).to_le_bytes())?;
        f.write_all(&self.ch.to_le_bytes())?;
        f.write_all(&self.sr.to_le_bytes())?;
        f.write_all(&(self.sr * self.ch as u32 * bytes).to_le_bytes())?;
        f.write_all(&(self.ch * bytes as u16).to_le_bytes())?;
        f.write_all(&(bytes as u16 * 8).to_le_bytes())?;
        f.write_all(b"data")?;
        f.write_all(&data.to_le_bytes())
    }

    /// Interleaved frames.
    pub fn write(&mut self, l: &[f32], r: &[f32]) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(l.len() * 2 * self.fmt.bytes());
        for (&a, &b) in l.iter().zip(r) {
            for v in [a, b].into_iter().take(self.ch as usize) {
                match self.fmt {
                    Format::Float32 => buf.extend_from_slice(&v.to_le_bytes()),
                    Format::Pcm16 => buf.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()),
                    Format::Pcm24 => buf.extend_from_slice(&((v.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32).to_le_bytes()[..3]),
                }
            }
        }
        self.frames += l.len() as u64;
        self.f.write_all(&buf)
    }

    pub fn bytes_written(&self) -> u64 {
        44 + self.frames * self.ch as u64 * self.fmt.bytes() as u64
    }

    /// Patch the sizes in the header and flush.
    pub fn finish(mut self) -> std::io::Result<()> {
        self.f.flush()?;
        let mut file = self.f.into_inner().map_err(|e| e.into_error())?;
        let bytes = self.fmt.bytes() as u64;
        let data = (self.frames * self.ch as u64 * bytes).min(u32::MAX as u64 - 64) as u32;
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&(36 + data).to_le_bytes())?;
        file.seek(SeekFrom::Start(40))?;
        file.write_all(&data.to_le_bytes())?;
        file.flush()
    }
}

pub struct WavData {
    pub sr: u32,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    pub src_channels: u16,
}

impl WavData {
    /// First two channels (a mono file is duplicated into both).
    pub fn from_channels(sr: u32, mut ch: Vec<Vec<f32>>) -> Self {
        let src_channels = ch.len() as u16;
        ch.truncate(2);
        let r = if ch.len() > 1 { ch.pop().unwrap() } else { ch[0].clone() };
        Self { sr, l: ch.swap_remove(0), r, src_channels }
    }
}

/// Most samples (all channels together) that are read into memory: about 1.2 GB of floats.
pub const MAX_SAMPLES: u64 = 300_000_000;
/// Most channels and the range of sample rates a file may declare.
pub const MAX_CHANNELS: u16 = 64;
pub const SR_RANGE: std::ops::RangeInclusive<u32> = 2000..=768_000;

/// Every channel of the file as its own vector.
pub fn read_all(path: &std::path::Path) -> Result<(u32, Vec<Vec<f32>>), String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hdr = [0u8; 12];
    file.read_exact(&mut hdr).map_err(|e| e.to_string())?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let (mut tag, mut bits, mut ch, mut sr) = (0u16, 0u16, 0u16, 0u32);
    loop {
        let mut ch_hdr = [0u8; 8];
        if file.read_exact(&mut ch_hdr).is_err() {
            return Err("no data chunk".into());
        }
        let size = u32::from_le_bytes([ch_hdr[4], ch_hdr[5], ch_hdr[6], ch_hdr[7]]) as u64;
        match &ch_hdr[0..4] {
            b"fmt " => {
                if !(16..=1024).contains(&size) {
                    return Err("bad fmt chunk".into());
                }
                let mut b = vec![0u8; size as usize];
                file.read_exact(&mut b).map_err(|e| e.to_string())?;
                if b.len() < 16 {
                    return Err("bad fmt chunk".into());
                }
                tag = u16::from_le_bytes([b[0], b[1]]);
                ch = u16::from_le_bytes([b[2], b[3]]);
                sr = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                bits = u16::from_le_bytes([b[14], b[15]]);
                if tag == 0xFFFE && b.len() >= 26 {
                    tag = u16::from_le_bytes([b[24], b[25]]);
                }
                if size % 2 == 1 {
                    file.seek(SeekFrom::Current(1)).ok();
                }
            }
            b"data" => {
                if ch == 0 || sr == 0 {
                    return Err("data before fmt".into());
                }
                if ch > MAX_CHANNELS {
                    return Err(format!("{ch} channels are not supported"));
                }
                if !SR_RANGE.contains(&sr) {
                    return Err(format!("unsupported sample rate ({sr} Hz)"));
                }
                let bytes = (bits / 8) as usize;
                if bytes == 0 || !matches!((tag, bits), (1, 8) | (1, 16) | (1, 24) | (1, 32) | (3, 32) | (3, 64)) {
                    return Err(format!("unsupported format (tag {tag}, {bits} bit)"));
                }
                let avail = file.metadata().map(|m| m.len()).unwrap_or(size).saturating_sub(file.stream_position().unwrap_or(0));
                let size = size.min(avail);
                let frame_bytes = bytes * ch as usize;
                let frames = size as usize / frame_bytes;
                if frames as u64 * ch as u64 > MAX_SAMPLES {
                    return Err("the file is too large to load".into());
                }
                let mut out: Vec<Vec<f32>> = (0..ch).map(|_| Vec::with_capacity(frames)).collect();
                let mut buf = vec![0u8; frame_bytes * 16384];
                let mut left = frames;
                let mut rd = std::io::BufReader::new(&mut file);
                while left > 0 {
                    let n = left.min(16384);
                    let chunk = &mut buf[..n * frame_bytes];
                    rd.read_exact(chunk).map_err(|e| e.to_string())?;
                    for fr in chunk.chunks_exact(frame_bytes) {
                        let s = |c: usize| -> f32 {
                            let o = &fr[c * bytes..(c + 1) * bytes];
                            match (tag, bits) {
                                (1, 8) => (o[0] as f32 - 128.0) / 128.0,
                                (1, 16) => i16::from_le_bytes([o[0], o[1]]) as f32 / 32768.0,
                                (1, 24) => (i32::from_le_bytes([0, o[0], o[1], o[2]]) >> 8) as f32 / 8_388_608.0,
                                (1, 32) => i32::from_le_bytes([o[0], o[1], o[2], o[3]]) as f32 / 2_147_483_648.0,
                                (3, 32) => f32::from_le_bytes([o[0], o[1], o[2], o[3]]),
                                _ => f64::from_le_bytes([o[0], o[1], o[2], o[3], o[4], o[5], o[6], o[7]]) as f32,
                            }
                        };
                        for (c, o) in out.iter_mut().enumerate() {
                            o.push(s(c));
                        }
                    }
                    left -= n;
                }
                if out[0].is_empty() {
                    return Err("empty data chunk".into());
                }
                return Ok((sr, out));
            }
            _ => {
                file.seek(SeekFrom::Current((size + (size & 1)) as i64)).map_err(|e| e.to_string())?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_formats() {
        let dir = std::env::temp_dir();
        for fmt in Format::ALL {
            let path = dir.join(format!("visuscope_wav_{fmt:?}.wav"));
            let l: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.05).sin() * 0.8).collect();
            let r: Vec<f32> = l.iter().map(|v| -v * 0.5).collect();
            let mut w = WavWriter::create(&path, fmt, 2, 44100).unwrap();
            w.write(&l[..400], &r[..400]).unwrap();
            w.write(&l[400..], &r[400..]).unwrap();
            w.finish().unwrap();
            let d = WavData::from_channels(read_all(&path).unwrap().0, read_all(&path).unwrap().1);
            assert_eq!((d.sr, d.l.len()), (44100, 1000));
            let tol = match fmt {
                Format::Pcm16 => 1e-4,
                Format::Pcm24 => 1e-6,
                Format::Float32 => 0.0,
            };
            for i in 0..1000 {
                assert!((d.l[i] - l[i]).abs() <= tol + 1e-9 && (d.r[i] - r[i]).abs() <= tol + 1e-9, "{fmt:?} sample {i}");
            }
            let _ = std::fs::remove_file(path);
        }
    }
}
