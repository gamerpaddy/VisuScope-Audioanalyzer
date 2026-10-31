# VisuScope

Real-time audio analyzer for Windows. Captures from an input device or from WASAPI loopback and shows spectrum, spectrogram, 3D spectrum, stereo field and oscilloscope, with distortion measurements and a signal generator.

> **This is not REW (Room EQ Wizard).** It is not intended to be or replace it and has no affiliation with it.

![Overview](docs/overview.png)

## Features

**Input**
- Microphone, line input, or loopback of any playback device
- Any device channel as L and R, with a level meter per channel
- Microphone calibration files (0 / 90 degrees, e.g. UMIK-1) correct the spectrum views
- Reconnects when the device format changes
- Plays and analyzes audio files (WAV, FLAC, MP3, Ogg Vorbis, AAC/M4A, ALAC, MKV) with optional sound output
- Records to WAV (16/24 bit, 32 bit float) with pre-roll

**Views**
- Spectrum: FFT 1024 to 262144, many window functions, live/average/max/min traces, cursors, snapshots, CSV export
- Spectrogram and waterfall
- 3D spectrum
- Stereo field: goniometer, correlation, balance, mid/side, correlation per frequency
- Oscilloscope: auto, normal, single and roll trigger, HF reject, adjustable trigger position and level
- Octave analyzer, 1/1 to 1/24, A/C weighting, target curve

![Analysis tools](docs/tools.png)

**Measurements**
- THD, THD+N, SNR, SINAD, SFDR, ENOB, independent of the display window
- Voltage calibration (dBFS, dBV, dBu)
- Frequency response by sweep: magnitude, phase, group delay, coherence
- Impulse response with RT60, EDT, C50, C80, D50
- Loudness (EBU R128), true peak, PPM, VU
- Live transfer function: magnitude, phase and coherence between a reference and a measured channel while music or noise plays, with automatic delay compensation
- Sound level meter: A/C/Z, Fast/Slow/Impulse, Leq, Lmax, peak, L10/L50/L90, calibrator or specification calibration
- Distortion sweeps: THD / THD+N vs frequency and level, IMD (SMPTE, CCIF) vs level, harmonics 2 to 6 from one sweep (Farina)
- Room acoustics: RT60 / EDT / clarity per octave band, waterfall (CSD), STI and %ALcons, room modes with theoretical axial modes
- Peak list with note names, tuner
- Signal statistics: histogram, clipping, bit depth use, trends
- Reference curves and limit masks with pass/fail
- CSV logger with threshold alarms

![Sweep measurement](docs/response.png)

**Generator**
- Sine, square, triangle, saw, pulse, impulse, sweeps, multitone, SMPTE and CCIF IMD
- White, pink, brown, blue and violet noise
- Harmonics, phase, jitter, bit depth with dither

**Automation and output**
- Test sequences: a small script drives the generator, measures, sweeps and checks limits (pass/fail); `visuscope --run test.vss --report result.txt` runs it from the command line (exit code 0 pass, 1 fail, 2 error)
- Measurement report as HTML (print to PDF), response export as FRD / REW text, impulse response as WAV
- EQ design: parametric filters fitted to a measurement, exported as Equalizer APO text, biquads or a linear / minimum phase FIR

**Interface**
- Stored traces: name, recolour, overlay, show relative to one trace, average, save and load
- Fractional-octave smoothing of the spectrum
- Floating windows, auto tiling, saved layouts (Ctrl+1 to 9), pop-out windows
- Settings are remembered between runs
- PNG screenshots

## Download

Prebuilt Windows builds are on the [Releases](../../releases) page.

## Build

Needs Rust 1.85 or newer.

```
cargo build --release
```

The executable is `target/release/visuscope.exe`. Only tested on Windows.

Settings and logs are stored in `%APPDATA%\visuscope`. Recordings and CSV logs go to `Documents\VisuScope`.

## License

[Unlicense](LICENSE), public domain.

This project was made using AI.
