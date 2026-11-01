//! Opens most audio files: WAV through the built-in reader, everything else through Symphonia
//! (MP3, FLAC, Ogg Vorbis, AAC / M4A / MP4, ALAC, MKV / WebM audio, AIFF-less PCM containers).

use super::wavio::{self, WavData};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// File extensions offered by the open dialog.
pub const EXTENSIONS: &[&str] = &["wav", "flac", "mp3", "ogg", "oga", "m4a", "mp4", "aac", "alac", "mkv", "mka", "webm", "caf", "w64"];

pub fn read(path: &std::path::Path) -> Result<WavData, String> {
    read_all(path).map(|(sr, ch)| WavData::from_channels(sr, ch))
}

/// Sample rate and every channel of the file.
pub fn read_all(path: &std::path::Path) -> Result<(u32, Vec<Vec<f32>>), String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if path.is_dir() {
        return Err("that is a folder, not a file".into());
    }
    if ext == "wav" {
        // fast path; falls back to the general decoder for ADPCM and other exotic WAVs
        match std::panic::catch_unwind(|| wavio::read_all(path)) {
            Ok(Ok(d)) => return Ok(d),
            Ok(Err(e)) if e.contains("too large") || e.contains("channels are not supported") || e.contains("sample rate") => return Err(e),
            _ => {}
        }
    }
    // a decoder that panics on a damaged file must not take the program down with it
    let ext2 = ext.clone();
    let (sr, ch) = std::panic::catch_unwind(|| decode(path, &ext2)).unwrap_or_else(|_| Err("the decoder failed on this file (damaged?)".into()))?;
    if !wavio::SR_RANGE.contains(&sr) {
        return Err(format!("unsupported sample rate ({sr} Hz)"));
    }
    if ch.len() > wavio::MAX_CHANNELS as usize {
        return Err(format!("{} channels are not supported", ch.len()));
    }
    Ok((sr, ch))
}

fn decode(path: &std::path::Path, ext: &str) -> Result<(u32, Vec<Vec<f32>>), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()).map_err(|e| format!("unknown or unsupported format ({e})"))?;
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL).ok_or("no audio track")?;
    let id = track.id;
    let mut dec = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| format!("unsupported codec ({e})"))?;
    let (mut sr, mut chans) = (track.codec_params.sample_rate.unwrap_or(0), track.codec_params.channels.map(|c| c.count()).unwrap_or(0));
    let mut out: Vec<Vec<f32>> = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => {
                if out.first().map_or(true, |c| c.is_empty()) {
                    return Err(format!("read error: {e}"));
                }
                break;
            }
        };
        if packet.track_id() != id {
            continue;
        }
        match dec.decode(&packet) {
            Ok(audio) => {
                let spec = *audio.spec();
                sr = spec.rate;
                chans = spec.channels.count().max(1);
                if buf.as_ref().map(|b| b.capacity() < audio.capacity() * chans).unwrap_or(true) {
                    buf = Some(SampleBuffer::new(audio.capacity() as u64, spec));
                }
                let b = buf.as_mut().unwrap();
                b.copy_interleaved_ref(audio);
                if out.len() != chans {
                    // a stream that changes its channel count mid-file: keep what we have, start over only if empty
                    if out.first().map_or(true, |c| c.is_empty()) {
                        out = vec![Vec::new(); chans];
                    } else {
                        continue;
                    }
                }
                for fr in b.samples().chunks_exact(chans) {
                    for (c, o) in out.iter_mut().enumerate() {
                        o.push(fr[c]);
                    }
                }
                if out.len() as u64 * out[0].len() as u64 > wavio::MAX_SAMPLES {
                    return Err("the file is too large to load".into());
                }
            }
            // a corrupt packet: skip it
            Err(Error::DecodeError(_)) => continue,
            Err(e) => {
                if out.first().map_or(true, |c| c.is_empty()) {
                    return Err(format!("decode error: {e}"));
                }
                break;
            }
        }
    }
    if out.first().map_or(true, |c| c.is_empty()) || sr == 0 {
        return Err("no audio decoded".into());
    }
    let _ = chans;
    Ok((sr, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::wavio::{Format, WavWriter};

    /// `VISUSCOPE_TEST_FILE=path cargo test decode_any_file -- --nocapture` decodes a real MP3 / FLAC / ...
    #[test]
    fn decode_any_file() {
        let Some(p) = std::env::var_os("VISUSCOPE_TEST_FILE") else { return };
        let d = read(std::path::Path::new(&p)).expect("decode");
        let peak = d.l.iter().fold(0f32, |m, v| m.max(v.abs()));
        println!("{:?}: {} Hz, {} ch, {:.1} s, peak {peak:.3}", p, d.sr, d.src_channels, d.l.len() as f32 / d.sr as f32);
        assert!(d.l.len() > 1000 && peak > 0.001 && peak <= 1.5);
    }

    /// A small valid stereo 16 bit WAV (`frames` frames) as bytes.
    fn wav_bytes(frames: usize, sr: u32) -> Vec<u8> {
        let mut b = Vec::new();
        let data = frames * 4;
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&((36 + data) as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        for v in [1u16, 2] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&sr.to_le_bytes());
        b.extend_from_slice(&(sr * 4).to_le_bytes());
        for v in [4u16, 16] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data as u32).to_le_bytes());
        for i in 0..frames {
            let v = ((i as f32 * 0.1).sin() * 12000.0) as i16;
            b.extend_from_slice(&v.to_le_bytes());
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("visuscope_decode_edge_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn odd_file_names_still_load() {
        for name in ["plain.wav", "with spaces in it.wav", "UPPER.WAV", "ünïcödé 日本語 😀.wav", ".hidden.wav", "double.wav.wav", "100% sure & (more) [x] #1 'q'.wav", &format!("{}.wav", "x".repeat(150))] {
            let p = scratch(name);
            std::fs::write(&p, wav_bytes(300, 44100)).unwrap();
            let r = read_all(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((r.0, r.1.len(), r.1[0].len()), (44100, 2, 300), "{name}");
            let _ = std::fs::remove_file(p);
        }
        // not a file, missing, no extension, wrong extension: errors, not panics
        assert!(read_all(&std::env::temp_dir()).is_err());
        assert!(read_all(std::path::Path::new("Z:\\does\\not\\exist.wav")).is_err());
        let noext = scratch("noextension");
        std::fs::write(&noext, wav_bytes(100, 48000)).unwrap();
        let _ = read_all(&noext);
        let wrong = scratch("really_a_wav.mp3");
        std::fs::write(&wrong, wav_bytes(100, 48000)).unwrap();
        let _ = read_all(&wrong);
    }

    #[test]
    fn damaged_files_give_errors_not_panics() {
        let good = wav_bytes(400, 48000);
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let mut cases: Vec<(String, Vec<u8>)> = vec![("empty".into(), Vec::new()), ("one byte".into(), vec![0x52]), ("riff only".into(), b"RIFF".to_vec()), ("zeros".into(), vec![0; 4096]), ("ff".into(), vec![0xff; 4096])];
        // every truncation of the header and some more
        for n in (0..70).chain([100, 200, 399]) {
            cases.push((format!("truncated {n}"), good[..n.min(good.len())].to_vec()));
        }
        // random damage
        for k in 0..400 {
            let mut b = good.clone();
            for _ in 0..1 + rng.next() % 5 {
                let i = (rng.next() % b.len() as u64) as usize;
                b[i] = rng.next() as u8;
            }
            if k % 3 == 0 {
                b.truncate((rng.next() % b.len() as u64) as usize);
            }
            cases.push((format!("mutated {k}"), b));
        }
        // header values that would need absurd amounts of memory
        for (what, at, v) in [("huge fmt size", 16usize, u32::MAX), ("huge data size", 40, u32::MAX), ("zero rate", 24, 0), ("tiny rate", 24, 1), ("huge rate", 24, u32::MAX)] {
            let mut b = good.clone();
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
            cases.push((what.into(), b));
        }
        for (what, at, v) in [("65535 channels", 22usize, 65535u16), ("zero channels", 22, 0), ("0 bits", 34, 0), ("odd bits", 34, 13)] {
            let mut b = good.clone();
            b[at..at + 2].copy_from_slice(&v.to_le_bytes());
            cases.push((what.into(), b));
        }
        // random bytes under the names of every supported format
        let junk: Vec<u8> = (0..20000).map(|_| rng.next() as u8).collect();
        for ext in EXTENSIONS {
            cases.push((format!("junk .{ext}"), junk.clone()));
        }
        let t0 = std::time::Instant::now();
        for (i, (what, bytes)) in cases.iter().enumerate() {
            let ext = if what.starts_with("junk .") { what.trim_start_matches("junk ").to_string() } else { ".wav".into() };
            let p = scratch(&format!("case{i}{ext}"));
            std::fs::write(&p, bytes).unwrap();
            let r = std::panic::catch_unwind(|| read_all(&p));
            let _ = std::fs::remove_file(&p);
            match r {
                Err(_) => panic!("{what}: panicked"),
                Ok(Ok((sr, ch))) => {
                    // whatever loads must be sane
                    assert!(wavio::SR_RANGE.contains(&sr) && !ch.is_empty() && ch.len() <= wavio::MAX_CHANNELS as usize && !ch[0].is_empty(), "{what}: odd result {sr} Hz {} ch", ch.len());
                }
                Ok(Err(_)) => {}
            }
        }
        assert!(t0.elapsed().as_secs() < 60, "the damaged files took too long");
    }

    #[test]
    fn decodes_wav_through_both_paths() {
        let path = std::env::temp_dir().join("visuscope_decode_test.wav");
        let l: Vec<f32> = (0..4000).map(|i| (i as f32 * 0.03).sin() * 0.5).collect();
        let mut w = WavWriter::create(&path, Format::Pcm16, 2, 32000).unwrap();
        w.write(&l, &l).unwrap();
        w.finish().unwrap();
        let fast = read(&path).unwrap();
        // force the Symphonia path
        let slow = WavData::from_channels(32000, decode(&path, "wav").unwrap().1);
        assert_eq!((fast.sr, fast.l.len()), (32000, 4000));
        assert_eq!((slow.sr, slow.l.len()), (32000, 4000));
        for i in (0..4000).step_by(97) {
            assert!((fast.l[i] - slow.l[i]).abs() < 1e-4);
        }
        let _ = std::fs::remove_file(path);
        assert!(read(std::path::Path::new("nonexistent.mp3")).is_err());
    }
}
