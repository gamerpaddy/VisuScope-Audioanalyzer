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
    if ext == "wav" {
        // fast path; falls back to the general decoder for ADPCM and other exotic WAVs
        if let Ok(d) = wavio::read_all(path) {
            return Ok(d);
        }
    }
    decode(path, &ext)
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
