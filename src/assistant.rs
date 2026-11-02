//! The assistant: type what you want to measure ("frequency response, impulse response and distortion with a sweep on
//! input mic1") and it sets the program up for it: the input, the windows and their layout, the sweep and generator
//! settings, and, if asked, starts the measurements one after the other. It works offline with keywords and numbers, no
//! service involved, and always shows its plan before it changes anything.

use crate::app::App;
use crate::tonegen::{NoiseKind, Wave};
use crate::windows::{win_short, BUILTIN, WIN_COUNT};
use eframe::egui::{self, vec2, Align2, Color32, RichText};
use std::collections::VecDeque;
use std::time::Instant;

/// (button, sentence): the buttons of the assistant window
const EXAMPLES: [(&str, &str); 12] = [
    ("Response + impulse + distortion", "measure frequency response, impulse response and distortion with a sweep on input mic1"),
    ("THD and IMD", "check THD and IMD of the line input"),
    ("Room acoustics", "room acoustics: RT60 and clarity with a sweep from 50 hz to 16 khz"),
    ("EQ for a speaker", "measure the frequency response with a sweep and design an EQ to correct it"),
    ("Spectrum + spectrogram", "show the spectrum and spectrogram of channel 2"),
    ("Loudness", "measure the loudness of the music"),
    ("Sound level", "measure the sound level SPL with a meter"),
    ("Stereo image", "show the stereo image with goniometer and correlation"),
    ("Tone + monitor", "generate a 1 khz tone at -20 db and monitor it"),
    ("Transfer function", "measure the transfer function while music plays"),
    ("Null test", "null test of two signals"),
    ("Deutsch: Frequenzgang", "Frequenzgang, Impulsantwort und Verzerrung mit Sweep am Eingang Mikrofon 1 messen"),
];

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// device index (in the list of inputs) and channel (0 based)
    Input { dev: Option<usize>, label: String, ch: Option<usize> },
    Sweep { f0: Option<f32>, f1: Option<f32>, secs: Option<f32>, level: Option<f32>, averages: Option<usize> },
    DistMode(usize),
    Windows(Vec<&'static str>),
    Tone { wave: Wave, noise: Option<NoiseKind>, freq: Option<f32>, level: Option<f32> },
    Monitor,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Run {
    /// the sweep of the Response window (also gives the impulse response and the swept harmonics)
    Sweep,
    /// stepped distortion measurement: 0 THD vs frequency, 1 THD vs level, 2 IMD vs level
    Stepped(usize),
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub title: String,
    pub actions: Vec<Action>,
    pub runs: Vec<Run>,
    pub notes: Vec<String>,
}

impl Action {
    pub fn describe(&self) -> String {
        match self {
            Action::Input { label, ch, .. } => match ch {
                Some(c) => format!("Input: {label}, channel {}", c + 1),
                None => format!("Input: {label}"),
            },
            Action::Sweep { f0, f1, secs, level, averages } => {
                let mut v = Vec::new();
                if let (Some(a), Some(b)) = (f0, f1) {
                    v.push(format!("{} to {}", hz(*a), hz(*b)));
                } else if let Some(a) = f0 {
                    v.push(format!("from {}", hz(*a)));
                } else if let Some(b) = f1 {
                    v.push(format!("up to {}", hz(*b)));
                }
                if let Some(s) = secs {
                    v.push(format!("{s:.1} s long"));
                }
                if let Some(l) = level {
                    v.push(format!("{l:.0} dBFS"));
                }
                if let Some(a) = averages {
                    v.push(format!("{a} averages"));
                }
                format!("Sweep: {}", v.join(", "))
            }
            Action::DistMode(m) => format!("Distortion window: {}", ["THD vs frequency", "THD vs level", "IMD vs level", "swept harmonics (from the same sweep)"][(*m).min(3)]),
            Action::Windows(w) => format!("Windows: {}", w.join(", ")),
            Action::Tone { wave, noise, freq, level } => {
                let name = match noise {
                    Some(n) => format!("{n:?} noise"),
                    None => wave.name().to_string(),
                };
                let mut s = format!("Generator: {name}");
                if let Some(f) = freq {
                    s += &format!(" {}", hz(*f));
                }
                if let Some(l) = level {
                    s += &format!(" at {l:.0} dBFS");
                }
                s
            }
            Action::Monitor => "Monitor: on (plays the input on the output chosen in the side bar)".into(),
        }
    }
}

impl Run {
    pub fn describe(&self) -> &'static str {
        match self {
            Run::Sweep => "run the sweep (frequency response, impulse response, swept harmonics)",
            Run::Stepped(0) => "run the stepped THD vs frequency measurement",
            Run::Stepped(1) => "run the stepped THD vs level measurement",
            Run::Stepped(_) => "run the stepped IMD measurement",
        }
    }
}

fn hz(f: f32) -> String {
    if f >= 1000.0 { format!("{} kHz", (f / 100.0).round() / 10.0) } else { format!("{f:.0} Hz") }
}

// ---------------------------------------------------------------------- reading the sentence

/// Lower case without accents, so that "réponse", "distorsión" and "Übertragung" are matched by plain keywords.
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .flat_map(|c| -> Vec<char> {
            match c {
                'á' | 'à' | 'â' | 'ä' | 'ã' => vec!['a'],
                'é' | 'è' | 'ê' | 'ë' => vec!['e'],
                'í' | 'ì' | 'î' | 'ï' => vec!['i'],
                'ó' | 'ò' | 'ô' | 'ö' | 'õ' => vec!['o'],
                'ú' | 'ù' | 'û' | 'ü' => vec!['u'],
                'ç' => vec!['c'],
                'ñ' => vec!['n'],
                'ß' => vec!['s', 's'],
                c => vec![c],
            }
        })
        .collect()
}

/// Words a typo is corrected to (one wrong, missing or extra letter; two for long words).
const VOCAB: &[&str] = &[
    "frequency", "response", "impulse", "distortion", "harmonic", "harmonics", "loudness", "spectrum", "spectrogram", "stereo", "microphone", "measure", "reverb", "acoustics", "generate", "generator", "monitor", "oscilloscope", "waveform", "record", "timeline", "equalizer", "octave", "transfer", "intermodulation", "correlation", "goniometer", "waterfall", "analyse", "analyze", "frequenzgang", "impulsantwort", "verzerrung", "lautheit", "spektrum", "spektrogramm", "mikrofon", "messen", "klirrfaktor", "nachhall", "raumakustik", "frecuencia", "respuesta", "distorsion", "sonoridad", "espectro", "frequence", "reponse", "impulsionnelle", "acoustique",
];

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A misspelt word becomes the keyword it is closest to; everything else stays.
fn correct(word: &str) -> String {
    if word.len() < 6 || !word.chars().all(|c| c.is_alphabetic()) || VOCAB.contains(&word) {
        return word.to_string();
    }
    let limit = if word.len() >= 9 { 2 } else { 1 };
    VOCAB.iter().filter(|v| v.len().abs_diff(word.len()) <= limit).map(|v| (edit_distance(word, v), *v)).filter(|(d, _)| *d <= limit).min_by_key(|(d, _)| *d).map(|(_, v)| v.to_string()).unwrap_or_else(|| word.to_string())
}

/// Lower case words and numbers: letters and digits are separated ("mic1" -> "mic", "1"), "20hz" -> "20", "hz"; a minus
/// sign in front of a number stays with it.
fn words(text: &str) -> Vec<String> {
    let chars: Vec<char> = fold(text).chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let prev = if i > 0 { chars[i - 1] } else { ' ' };
        let next = chars.get(i + 1).copied().unwrap_or(' ');
        if c.is_alphanumeric() || c == '/' && prev.is_ascii_digit() && next.is_ascii_digit() {
            if (prev.is_alphabetic() && c.is_ascii_digit()) || (prev.is_ascii_digit() && c.is_alphabetic()) {
                out.push(' ');
            }
            out.push(c);
        } else if c == '.' && prev.is_ascii_digit() && next.is_ascii_digit() {
            out.push(c);
        } else if c == '-' && next.is_ascii_digit() && !prev.is_alphanumeric() {
            out.push(' ');
            out.push(c);
        } else {
            out.push(' ');
        }
    }
    out.split_whitespace().map(|s| s.to_string()).collect()
}

fn number(s: &str) -> Option<f32> {
    s.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// `number [unit]` at a position: the value in Hz for frequency units.
fn freq_at(w: &[String], i: usize) -> Option<(f32, usize)> {
    let v = number(w.get(i)?)?;
    match w.get(i + 1).map(|s| s.as_str()) {
        Some("khz" | "k") => Some((v * 1000.0, 2)),
        Some("hz") => Some((v, 2)),
        _ => Some((v, 1)),
    }
}

fn has_unit(w: &[String], i: usize, units: &[&str]) -> bool {
    w.get(i + 1).is_some_and(|u| units.contains(&u.as_str()))
}

/// "from 20 hz to 20 khz", "50 to 16 khz", "between 100 and 8000 hz"
fn freq_range(w: &[String]) -> Option<(f32, f32)> {
    for i in 0..w.len() {
        if !matches!(w[i].as_str(), "from" | "between" | "von" | "ab" | "zwischen" | "de" | "desde" | "entre") && !(number(&w[i]).is_some() && w[i..].iter().take(4).any(|t| TO.contains(&t.as_str()))) {
            continue;
        }
        let start = if number(&w[i]).is_some() { i } else { i + 1 };
        // a "from" that is not followed by a number ("from mic 1") is not a range: look on
        let Some((a, la)) = freq_at(w, start) else { continue };
        let mut j = start + la;
        if w.get(j).is_some_and(|s| TO.contains(&s.as_str()) || matches!(s.as_str(), "and" | "und" | "et" | "y")) {
            j += 1;
        }
        let Some((b, _)) = freq_at(w, j) else { continue };
        // "20 to 20 khz": only the second has a unit
        let khz_b = has_unit(w, j, &["khz", "k"]);
        let khz_a = has_unit(w, start, &["khz", "k"]);
        let a = if !khz_a && !has_unit(w, start, &["hz"]) && khz_b && a * 1000.0 < b { a * 1000.0 } else { a };
        if a > 0.0 && b > a {
            return Some((a, b));
        }
    }
    None
}

const TO: &[&str] = &["to", "until", "through", "bis", "a", "hasta", "au", "jusqu"];

const STOP: &[&str] = &[
    "die", "der", "das", "den", "dem", "und", "mit", "von", "bis", "auf", "ein", "eine", "einen", "bitte", "messe", "messen", "messung", "mess", "pruefe", "starte", "zeige", "zeig", "eingang", "kanal", "frequenzgang", "impulsantwort", "verzerrung", "klirrfaktor", "lautheit", "spektrum", "spektrogramm", "raumakustik", "nachhall", "mikrofon", "mikro", "los", "les", "des", "une", "avec", "sur", "entree", "mesure", "mesurer", "reponse", "frequence", "distorsion", "sonie", "spectre", "acoustique", "bruit", "los", "las", "con", "una", "entrada", "canal", "medir", "mide", "respuesta", "frecuencia", "distorsion", "sonoridad", "espectro", "acustica", "ruido", "para", "por", "del", "que", "pour", "dans",
    "the", "and", "with", "from", "using", "use", "via", "input", "inputs", "measure", "measurement", "check", "show", "set", "setup", "please", "for", "of", "on", "a", "an", "at", "to", "me", "my", "all", "run", "start", "do", "test", "get", "make", "can", "you", "that", "this", "then", "also", "plus", "channel", "chan", "ch", "analysis", "analyse", "analyze", "sweep", "tone", "generate", "play", "frequency", "response", "impulse", "distortion", "thd", "imd", "rt60", "loudness", "room", "acoustics", "spectrum", "spectrogram", "scope", "stereo", "level", "length", "long", "average", "averages", "averaged", "seconds", "sec", "hz", "khz", "db", "dbfs", "sine", "square", "noise", "pink", "white", "monitor", "listen", "what", "how", "is", "are", "in", "out", "up", "over", "its", "it", "music", "sound", "signal", "device", "output", "clarity", "decay", "harmonics", "harmonic", "intermodulation", "meter", "null", "difference", "record", "report", "eq", "equaliser", "equalizer", "transfer", "function", "live", "volume", "lufs", "spl", "tuner", "pitch", "note", "waveform", "octave", "third",
];

fn contains_any(text: &str, keys: &[&str]) -> bool {
    keys.iter().any(|k| text.contains(k))
}

/// Everything the sentence asks for. `devs` are the labels of the input devices, `cur` the chosen one.
pub fn plan(text: &str, devs: &[String], cur: usize, graph_on: bool) -> Plan {
    let w: Vec<String> = words(text).into_iter().map(|s| correct(&s)).collect();
    let t = format!(" {} ", w.join(" "));
    let mut p = Plan::default();

    // ---- what is wanted (English, German, French and Spanish keywords; words are accent-free and typos are corrected)
    let imd = contains_any(&t, &["imd", "intermod", "twin tone", "smpte", "ccif", "zweiton"]);
    let dist = imd || contains_any(&t, &["distortion", "thd", "harmonic", "verzerrung", "klirr", "oberwellen", "distorsion", "armonic"]);
    let impulse = contains_any(&t, &["impulse", "rt 60", "rt60", "reverb", "decay", "clarity", "c 50", "c 80", "d 50", "edt", "impulsantwort", "nachhall", "abklingzeit", "reponse impulsionnelle", "reverberation", "respuesta al impulso", "reverberacion"]);
    let room = contains_any(&t, &["room", "acoustic", " modes", " sti ", "raumakustik", "raummoden", "acoustique", "acustica", "salle", "sala "]);
    let eq = contains_any(&t, &["equali", " eq ", "correct", "entzerr", "korrigier", "egalis", "ecualiz"]);
    let tuner = contains_any(&t, &["tuner", " tune ", "tune my", "pitch", "which note", "what note", " note", "frequency of", "what frequency", "which frequency", "peaks", "stimmgerat", "stimmen", "tonhohe", "accordeur", "afinador"]);
    let resp_strong = contains_any(&t, &["frequency response", "freq response", "magnitude", "group delay", "phase response", "bode", "frequenzgang", "amplitudengang", "phasengang", "reponse en frequence", "reponse frequentielle", "respuesta en frecuencia", "respuesta de frecuencia"]);
    // the bare words "frequency" and "response" also occur in "tone at frequency 440 hz": they only count when no generator is meant
    let resp_weak = contains_any(&t, &["frequency", "response"]);
    let loud = contains_any(&t, &["loudness", "lufs", "true peak", "r128", "ebu", "dynamic range", "lautheit", "lautstarke", "dynamikumfang", "sonie", "sonoridad"]);
    let spl = contains_any(&t, &[" spl", "sound level", "noise level", "db a", "dba", "level meter", "decibel", "schallpegel", "pegelmesser", "lautstarkemesser", "niveau sonore", "sonometre", "nivel sonoro", "sonometro"]);
    let transfer = contains_any(&t, &["transfer", "live response", "while music", "while playing", "ubertragungsfunktion", "uebertragungsfunktion", "fonction de transfert", "funcion de transferencia"]);
    let stereo = contains_any(&t, &["stereo", "goniometer", "correlation", "phase scope", "mono compat", "korrelation", "stereobild", "correlacion"]);
    let scope = contains_any(&t, &["scope", "waveform", "oscillo", "trigger", "kurvenform", "oszilloskop", "forme d onde", "oscilloscope", "osciloscopio", "forma de onda"]);
    let octave = contains_any(&t, &["octave", "1/3", "third", "rta", "oktave", "terz", "octava", "tiers d octave", "tercio"]);
    let spectrum = contains_any(&t, &["spectrum", "spectrogram", "waterfall", "fft", "analyse", "analyze", "watch", "look at", "frequencies", "spektrum", "spektrogramm", "wasserfall", "spectre", "espectro", "analys", "anzeigen", "zeige"]);
    let null = contains_any(&t, &["null test", " null ", "difference", "cancel", "subtract", "compare two", "nulltest", "differenz", "auslosch", "test nul", "prueba nula"]);
    let record = contains_any(&t, &["record", "timeline", "multitrack", " daw", "aufnahme", "aufnehmen", "mehrspur", "enregistr", "grabar", "grabacion"]);
    let report = contains_any(&t, &["report", " pdf", "export", "bericht", "rapport", "informe"]);
    let gen_words = contains_any(&t, &["generate", "generator", " tone", "white noise", "pink noise", "play a", "signal at", "erzeuge", "erzeugen", "sinuston", "rauschen", "signalgenerator", "genere", "generer", "ton von", "tono"]);
    let response = !tuner && (resp_strong || (resp_weak && !gen_words) || eq);
    let noise_meas = contains_any(&t, &["noise floor", "signal to noise", " snr", "sinad", "enob", "rauschabstand", "fremdspannung", "rapport signal", "relacion senal"]);
    let level_meas = !spl && !loud && contains_any(&t, &["level of", "peak level", " rms", "check the level", "pegel von", "niveau de", "nivel de"]);
    let monitor = contains_any(&t, &["monitor", "listen", " hear ", "headphone", "mithoren", "abhoren", "kopfhorer", "ecouter", "escuchar"]);
    let sweep_word = contains_any(&t, &["sweep", "wobbel", "balayage", "barrido"]);
    let verb_run = contains_any(&t, &["measure", " run ", " start", " do a", "perform", "test ", "check", "make a", "messe", "messen", "starte", "prufe", "durchfuhr", "mesur", "lance", "medir", "mide", "inicia", "ejecut"]);

    let mut wins: Vec<&'static str> = Vec::new();
    fn add(wins: &mut Vec<&'static str>, v: &[&'static str]) {
        for n in v {
            if !wins.contains(n) {
                wins.push(n);
            }
        }
    }
    let mut titles: Vec<&str> = Vec::new();
    let need_sweep = response || impulse || room || eq || (dist && !(t.contains("vs level") || t.contains("vs freq") || t.contains("stepped") || imd)) || (sweep_word && !gen_words && !dist);
    if response {
        add(&mut wins, &["Response"]);
        titles.push("frequency response");
    }
    if impulse || room {
        add(&mut wins, &["Impulse"]);
        titles.push(if room { "room acoustics" } else { "impulse response" });
    }
    if room {
        add(&mut wins, &["Octave"]);
    }
    if eq {
        add(&mut wins, &["Response", "EQ"]);
        titles.push("EQ design");
    }
    if dist {
        add(&mut wins, &["Distortion"]);
        titles.push(if imd { "distortion (IMD)" } else { "distortion" });
    }
    if need_sweep && wins.is_empty() {
        add(&mut wins, &["Response"]);
        titles.push("frequency response");
    }
    if noise_meas {
        add(&mut wins, &["Measure", "Spectrum"]);
        titles.push("noise floor and SNR");
    }
    if level_meas {
        add(&mut wins, &["Measure", "Scope"]);
        titles.push("level");
    }
    if loud {
        add(&mut wins, &["Loudness", "Spectrum"]);
        titles.push("loudness");
    }
    if spl {
        add(&mut wins, &["SPL", "Spectrum"]);
        titles.push("sound level");
    }
    if transfer {
        add(&mut wins, &["Transfer", "Spectrum"]);
        titles.push("transfer function");
    }
    if stereo {
        add(&mut wins, &["Stereo", "Scope", "Spectrum"]);
        titles.push("stereo image");
    }
    if scope && !stereo {
        add(&mut wins, &["Scope", "Spectrum"]);
        titles.push("waveform");
    }
    if octave && !room {
        add(&mut wins, &["Octave", "Spectrum"]);
        titles.push("octave bands");
    }
    if tuner {
        add(&mut wins, &["Peaks", "Spectrum"]);
        titles.push("peaks and tuner");
    }
    if spectrum {
        add(&mut wins, &["Spectrum", "Spectro", "Scope"]);
        titles.push("spectrum");
    }
    if null {
        add(&mut wins, &["Null test", "Spectrum"]);
        titles.push("null test");
    }
    if record {
        add(&mut wins, &["Timeline", "Scope", "Spectrum"]);
        titles.push("recording");
    }
    if report {
        add(&mut wins, &["Report"]);
    }
    if gen_words && !need_sweep {
        add(&mut wins, &["Generator", "Spectrum"]);
        titles.push("generator");
    }
    if monitor && wins.is_empty() {
        add(&mut wins, &["Spectrum"]);
    }
    // a measurement is easier to follow with the spectrum next to it
    if need_sweep || dist {
        add(&mut wins, &["Spectrum"]);
    }
    if graph_on && !wins.is_empty() {
        add(&mut wins, &["Routing"]);
    }

    if wins.is_empty() && !monitor {
        p.notes.push("I did not find a task in that. Try one of the examples below, or name what to measure: frequency response, impulse response / RT60, distortion (THD, IMD), loudness, sound level, spectrum, stereo, transfer function, null test, a generator tone ...".into());
        return p;
    }

    // ---- the input
    let chan = (0..w.len()).find_map(|i| {
        if matches!(w[i].as_str(), "mic" | "microphone" | "ch" | "chan" | "channel" | "input" | "in" | "line" | "inputs" | "mikrofon" | "mikro" | "kanal" | "eingang" | "micro" | "canal" | "entree" | "entrada" | "microfono") {
            w.get(i + 1).and_then(|n| n.parse::<usize>().ok()).filter(|n| (1..=64).contains(n))
        } else {
            None
        }
    });
    let query: Vec<&String> = w.iter().filter(|s| s.chars().all(|c| c.is_alphabetic()) && s.len() >= 3 && !STOP.contains(&s.as_str())).collect();
    let mentions_mic = w.iter().any(|s| matches!(s.as_str(), "mic" | "microphone" | "umik" | "mikrofon" | "mikro" | "micro" | "microfono"));
    let mentions_loop = w.iter().any(|s| matches!(s.as_str(), "loopback" | "system" | "cable" | "speakers" | "lautsprecher" | "systemton"));
    let mut dev_pick: Option<usize> = None;
    if !graph_on {
        let mut best = (0usize, None);
        for (i, d) in devs.iter().enumerate() {
            let l = d.to_lowercase();
            let mut score = query.iter().filter(|q| l.contains(q.as_str())).count() * 2;
            if mentions_mic && (l.contains("mic") || l.contains("umik")) {
                score += 2;
            }
            if mentions_loop && (l.contains("loopback") || l.contains("cable")) {
                score += 3;
            }
            if !mentions_loop && l.starts_with("[loopback]") {
                score = score.saturating_sub(1);
            }
            if score > best.0 {
                best = (score, Some(i));
            }
        }
        dev_pick = best.1;
        // only a sentence that talks about the input gets the "no such device" note
        let cue = mentions_mic || mentions_loop || chan.is_some() || w.iter().any(|s| matches!(s.as_str(), "input" | "inputs" | "eingang" | "kanal" | "channel" | "entree" | "entrada" | "canal" | "device" | "interface"));
        let named = cue && (!query.is_empty() || mentions_mic || mentions_loop);
        if named && dev_pick.is_none() {
            let asked = if query.is_empty() { if mentions_loop { "loopback".to_string() } else { "microphone".to_string() } } else { query.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ") };
            p.notes.push(format!("No input device matches \"{asked}\": the current input stays ({}).", devs.get(cur).cloned().unwrap_or_else(|| "none".into())));
        } else if let Some(i) = dev_pick {
            let ties = devs.iter().enumerate().filter(|(j, d)| *j != i && {
                let l = d.to_lowercase();
                query.iter().filter(|q| l.contains(q.as_str())).count() * 2 + if mentions_mic && l.contains("mic") { 2 } else { 0 } >= best.0
            }).count();
            if ties > 0 {
                p.notes.push(format!("{} input devices fit; I took \"{}\". Change it in the side bar if that is not the one.", ties + 1, devs[i]));
            }
        }
    } else if chan.is_some() || mentions_mic {
        p.notes.push("The routing graph is on: choose the input device and its channels in the Input node of the Routing window.".into());
    }
    if dev_pick.is_some() || chan.is_some() {
        let dev_ix = dev_pick.or(if devs.get(cur).is_some() { Some(cur) } else { None });
        let label = dev_ix.and_then(|i| devs.get(i).cloned()).unwrap_or_else(|| "current input".into());
        if !graph_on {
            p.actions.push(Action::Input { dev: dev_pick, label, ch: chan.map(|c| c - 1) });
        }
    }

    // ---- sweep settings
    if need_sweep {
        let range = freq_range(&w);
        let secs = (0..w.len()).find_map(|i| number(&w[i]).filter(|_| has_unit(&w, i, &["s", "sec", "secs", "second", "seconds", "sekunden", "sekunde", "sek", "secondes", "seconde", "segundos", "segundo"])).filter(|v| (0.5..=12.0).contains(v)));
        let level = (0..w.len()).find_map(|i| number(&w[i]).filter(|_| has_unit(&w, i, &["db", "dbfs"])).filter(|v| (-60.0..=0.0).contains(v)));
        let averages = (0..w.len()).find_map(|i| {
            let n = number(&w[i]).filter(|v| v.fract() == 0.0 && (1.0..=64.0).contains(v))? as usize;
            // "mic 2 averaged 4 times": the 2 belongs to the microphone
            if i > 0 && matches!(w[i - 1].as_str(), "mic" | "microphone" | "ch" | "chan" | "channel" | "input" | "inputs" | "in" | "line" | "mikrofon" | "kanal" | "eingang" | "canal" | "entrada" | "entree" | "micro") {
                return None;
            }
            let near = w.get(i + 1).is_some_and(|x| ["average", "avg", "mittel", "moyenn", "promed", "durchlauf", "wiederhol"].iter().any(|k| x.starts_with(k))) || (i > 0 && ["average", "avg", "mittel", "moyenn", "promed"].iter().any(|k| w[i - 1].starts_with(k)));
            near.then_some(n)
        });
        if range.is_some() || secs.is_some() || level.is_some() || averages.is_some() {
            p.actions.push(Action::Sweep { f0: range.map(|r| r.0), f1: range.map(|r| r.1), secs, level, averages });
        }
    }
    if dist {
        let mode = if imd { 2 } else if t.contains("vs level") || t.contains("versus level") { 1 } else if t.contains("vs freq") || t.contains("stepped") { 0 } else { 3 };
        p.actions.push(Action::DistMode(mode));
    }

    // ---- generator
    if gen_words && !need_sweep {
        let noise = if t.contains("pink") { Some(NoiseKind::Pink) } else if t.contains("white") { Some(NoiseKind::White) } else if t.contains("brown") { Some(NoiseKind::Brown) } else { None };
        let wave = if noise.is_some() || t.contains("noise") { Wave::Noise } else if t.contains("square") { Wave::Square } else if t.contains("triangle") { Wave::Triangle } else if t.contains("saw") { Wave::Saw } else { Wave::Sine };
        let freq = (0..w.len()).find_map(|i| number(&w[i]).filter(|_| has_unit(&w, i, &["hz", "khz"])).map(|v| if has_unit(&w, i, &["khz"]) { v * 1000.0 } else { v })).filter(|f| (1.0..=96000.0).contains(f));
        let level = (0..w.len()).find_map(|i| number(&w[i]).filter(|_| has_unit(&w, i, &["db", "dbfs"])).filter(|v| (-90.0..=0.0).contains(v)));
        p.actions.push(Action::Tone { wave, noise, freq, level });
    }
    if monitor {
        p.actions.push(Action::Monitor);
    }
    if !wins.is_empty() {
        p.actions.push(Action::Windows(wins.clone()));
    }

    // ---- what to run
    let mut runs = Vec::new();
    if need_sweep {
        runs.push(Run::Sweep);
    }
    if dist {
        if imd {
            runs.push(Run::Stepped(2));
        }
        if t.contains("vs level") || t.contains("versus level") {
            runs.push(Run::Stepped(1));
        } else if t.contains("vs freq") || t.contains("stepped") {
            runs.push(Run::Stepped(0));
        }
    }
    p.runs = runs;
    if !p.runs.is_empty() && !verb_run && !sweep_word {
        p.notes.push("Nothing is started until you press \"Set up and start\".".into());
    }
    if p.runs.iter().any(|r| matches!(r, Run::Sweep)) {
        p.notes.push("The sweep plays on the generator's output: connect it to the device under test, and that device to the input (or use a [Loopback] input).".into());
    }
    if p.runs.iter().any(|r| matches!(r, Run::Stepped(_))) {
        p.notes.push("The stepped distortion measurement needs the generator output fed back into the input.".into());
    }
    if titles.is_empty() {
        titles.push("setup");
    }
    let mut title = titles.join(", ");
    if let Some(Action::Input { label, ch, .. }) = p.actions.iter().find(|a| matches!(a, Action::Input { .. })) {
        title += &format!(" on {}{}", label, ch.map(|c| format!(" ch {}", c + 1)).unwrap_or_default());
    }
    let mut cs = title.chars();
    p.title = cs.next().map(|c| c.to_uppercase().collect::<String>() + cs.as_str()).unwrap_or_default();
    p
}

// ---------------------------------------------------------------------- state and window

#[derive(Default)]
pub struct State {
    pub open: bool,
    pub text: String,
    pub plan: Option<Plan>,
    queue: VecDeque<Run>,
    active: Option<(Run, Instant)>,
    pub log: Vec<String>,
    focus: bool,
    hook_done: bool,
    /// runs wait until the restarted input has settled (and delivers a sample rate)
    not_before: Option<Instant>,
    waiting_since: Option<Instant>,
}

impl App {
    fn assist_plan(&mut self) {
        let labels: Vec<String> = self.devices.iter().map(|d| d.label.clone()).collect();
        self.assist.plan = Some(plan(&self.assist.text, &labels, self.sel_dev, self.graph_on));
    }

    /// Carry out a plan: input, settings, windows; with `start` also queue its measurements.
    pub fn assist_apply(&mut self, p: &Plan, start: bool) {
        for a in &p.actions {
            match a {
                Action::Input { dev, ch, .. } => {
                    if let Some(d) = dev {
                        self.sel_dev = *d;
                    }
                    if let Some(c) = ch {
                        self.ch_a = (*c).min(crate::shared::MAX_CH - 1);
                        self.ch_b = self.ch_a;
                    }
                    self.start_selected();
                    // the stream needs a moment: a sweep that starts at once would find no input, or see it restart
                    self.assist.not_before = Some(Instant::now() + std::time::Duration::from_millis(1800));
                }
                Action::Sweep { f0, f1, secs, level, averages } => {
                    let sp = &mut self.sweep.p;
                    if let Some(v) = f0 {
                        sp.f0 = v.clamp(5.0, 2000.0);
                    }
                    if let Some(v) = f1 {
                        sp.f1 = v.clamp(1000.0, 96000.0);
                    }
                    if let Some(v) = secs {
                        sp.secs = *v;
                    }
                    if let Some(v) = level {
                        sp.amp_db = *v;
                    }
                    if let Some(v) = averages {
                        sp.averages = *v;
                    }
                }
                Action::DistMode(m) => {
                    if let Some(k) = (0..crate::tools::TOOL_COUNT).find(|&k| crate::tools::INFO[k].short == "Distortion") {
                        self.tools[k].command(&format!("mode\t{m}"));
                    }
                }
                Action::Windows(names) => {
                    for i in 0..WIN_COUNT {
                        let want = names.contains(&win_short(i));
                        self.wins[i].open = want;
                        if want {
                            self.wins[i].collapsed = false;
                        }
                        self.popped[i] = self.popped[i] && want;
                    }
                    self.need_tile = true;
                }
                Action::Tone { wave, noise, freq, level } => {
                    self.tg.wave = *wave;
                    if let Some(n) = noise {
                        self.tg.noise = *n;
                    }
                    if let Some(f) = freq {
                        self.tg.freq = *f;
                    }
                    if let Some(l) = level {
                        self.tg.level_db = [*l, *l];
                    }
                    self.tg.on = true;
                    if self.gen_handle.is_none() {
                        self.start_gen();
                    }
                }
                Action::Monitor => self.mon_on = true,
            }
        }
        // an opened window needs one frame before it can be used: runs start after the plan is in place
        self.assist.queue.clear();
        self.assist.active = None;
        if start {
            self.assist.queue.extend(p.runs.iter().copied());
        }
        self.assist.log.clear();
        self.assist.log.push(if start && !p.runs.is_empty() { "Set up. Starting the measurements ...".to_string() } else { "Set up.".to_string() });
        let _ = BUILTIN;
    }

    /// Every frame: start the queued measurements one after the other.
    pub fn assist_tick(&mut self) {
        // test hook: VISUSCOPE_ASSIST="what to do" opens the assistant with that sentence planned (VISUSCOPE_ASSIST_RUN=1 also sets it up)
        if !self.assist.hook_done && self.layout_ready {
            self.assist.hook_done = true;
            if let Ok(t) = std::env::var("VISUSCOPE_ASSIST") {
                self.assist.open = true;
                self.assist.text = t;
                self.assist_plan();
                if std::env::var_os("VISUSCOPE_ASSIST_RUN").is_some() {
                    if let Some(p) = self.assist.plan.clone() {
                        self.assist_apply(&p, false);
                    }
                }
            }
        }
        if let Some((run, t0)) = self.assist.active {
            let elapsed = t0.elapsed().as_secs_f32();
            let k = (0..crate::tools::TOOL_COUNT).find(|&k| crate::tools::INFO[k].short == "Distortion");
            let busy = match run {
                Run::Sweep => self.sweep.busy(),
                Run::Stepped(_) => k.is_some_and(|k| self.tools[k].status().is_some()),
            };
            // a measurement that never ends (an output that cannot start, a stalled input) must not block the queue for ever
            let limit = match run {
                Run::Sweep => (self.sweep.p.secs + self.sweep.p.tail) * self.sweep.p.averages.max(1) as f32 + 45.0,
                Run::Stepped(_) => 900.0,
            };
            if busy && elapsed > limit {
                if matches!(run, Run::Sweep) {
                    self.sweep.cancel();
                } else if let Some(k) = k {
                    self.tools[k].command("stop");
                }
                self.assist.log.push("That measurement took far too long and was stopped.".into());
                self.assist.active = None;
                return;
            }
            if !busy && elapsed > 1.0 {
                let problem = k.and_then(|k| self.tools[k].problem());
                let msg = match (&run, &self.sweep.state) {
                    (Run::Sweep, crate::tools::sweep::State::Failed(e)) => format!("Sweep failed: {e}"),
                    (Run::Sweep, crate::tools::sweep::State::Idle) => "The sweep did not run.".to_string(),
                    (Run::Sweep, _) => "Sweep finished: see the Response, Impulse and Distortion windows.".to_string(),
                    _ => match problem {
                        Some(p) => format!("Distortion measurement did not run: {p}"),
                        None => "Distortion measurement finished.".to_string(),
                    },
                };
                self.assist.log.push(msg);
                self.assist.active = None;
            }
            return;
        }
        if self.assist.queue.is_empty() {
            return;
        }
        // wait for the input to be running again after a device change (but not for ever)
        let waiting = self.assist.not_before.is_some_and(|t| Instant::now() < t) || self.sh.sr.load(std::sync::atomic::Ordering::Relaxed) == 0;
        if waiting {
            let since = *self.assist.waiting_since.get_or_insert_with(Instant::now);
            if since.elapsed().as_secs_f32() < 10.0 {
                return;
            }
        }
        self.assist.waiting_since = None;
        self.assist.not_before = None;
        let Some(run) = self.assist.queue.pop_front() else { return };
        self.assist.log.push(format!("Now: {}", run.describe()));
        match run {
            Run::Sweep => {
                // a tone left on would add to the sweep
                self.tg.on = false;
                self.sweep.start();
            }
            Run::Stepped(m) => {
                if let Some(k) = (0..crate::tools::TOOL_COUNT).find(|&k| crate::tools::INFO[k].short == "Distortion") {
                    self.tools[k].command(&format!("start\t{m}"));
                }
            }
        }
        self.assist.active = Some((run, Instant::now()));
    }

    /// The assistant's window (opened with the button at the right end of the top bar).
    pub fn assistant_ui(&mut self, ctx: &egui::Context) {
        if !self.assist.open {
            return;
        }
        let mut open = true;
        let mut do_plan = false;
        let mut apply: Option<bool> = None;
        egui::Window::new("Assistant").order(egui::Order::Foreground).open(&mut open).pivot(Align2::RIGHT_TOP).default_pos(ctx.content_rect().right_top() + vec2(-8.0, 34.0)).default_width(400.0).collapsible(false).resizable(true).show(ctx, |ui| {
            ui.label(RichText::new("Say what you want to measure or look at. I set up the input, windows, layout and settings, and can start the measurement.").small().weak());
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.assist.text).desired_width(ui.available_width() - 62.0).hint_text("e.g. frequency response and distortion with a sweep on input mic1"));
                if !self.assist.focus {
                    r.request_focus();
                    self.assist.focus = true;
                }
                if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) | ui.add_enabled(!self.assist.text.trim().is_empty(), egui::Button::new("Plan")).clicked() {
                    do_plan = true;
                }
            });
            ui.label(RichText::new("Examples: press one to plan it (the sentence is filled in, change it as you like)").small().weak());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for (label, sentence) in EXAMPLES {
                    if ui.small_button(label).on_hover_text(sentence).clicked() {
                        self.assist.text = sentence.to_string();
                        do_plan = true;
                    }
                }
            });
            ui.label(RichText::new("Understands English, German, French and Spanish keywords and fixes small typos. It is not an AI: it looks for task words, numbers and device names.").small().weak());
            if let Some(p) = self.assist.plan.clone() {
                ui.separator();
                if !p.title.is_empty() {
                    ui.label(RichText::new(&p.title).strong());
                }
                for a in &p.actions {
                    ui.label(RichText::new(format!("• {}", a.describe())).small());
                }
                for r in &p.runs {
                    ui.label(RichText::new(format!("• Then: {}", r.describe())).small().color(Color32::from_rgb(150, 220, 160)));
                }
                for n in &p.notes {
                    ui.label(RichText::new(n).small().color(Color32::from_rgb(255, 190, 100)));
                }
                if !p.actions.is_empty() {
                    ui.horizontal(|ui| {
                        if !p.runs.is_empty() && ui.button(RichText::new("Set up and start").strong()).clicked() {
                            apply = Some(true);
                        }
                        if ui.button(if p.runs.is_empty() { "Set up" } else { "Set up only" }).clicked() {
                            apply = Some(false);
                        }
                    });
                }
            }
            if !self.assist.log.is_empty() {
                ui.separator();
                for l in self.assist.log.iter().rev().take(6).rev() {
                    ui.label(RichText::new(l).small().weak());
                }
            }
        });
        if do_plan {
            self.assist_plan();
        }
        if let (Some(start), Some(p)) = (apply, self.assist.plan.clone()) {
            self.assist_apply(&p, start);
        }
        if !open {
            self.assist.open = false;
            self.assist.focus = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devs() -> Vec<String> {
        vec!["Microphone (UMIK-1)".into(), "Speakers (Realtek)".into(), "[Loopback] Speakers (Realtek)".into(), "CABLE Output (VB-Audio Virtual Cable)".into(), "Line (SSL 2+ USB)".into()]
    }

    fn windows(p: &Plan) -> Vec<&'static str> {
        p.actions.iter().find_map(|a| if let Action::Windows(w) = a { Some(w.clone()) } else { None }).unwrap_or_default()
    }

    #[test]
    fn the_example_sentence_sets_up_one_sweep_with_three_windows_on_the_microphone() {
        let p = plan("measure the frequency, impulse response and distortion with a sweep on input mic1", &devs(), 1, false);
        let w = windows(&p);
        for n in ["Response", "Impulse", "Distortion", "Spectrum"] {
            assert!(w.contains(&n), "{n} in {w:?}");
        }
        assert_eq!(p.runs, vec![Run::Sweep], "one sweep gives the response, the impulse response and the swept harmonics");
        assert!(p.actions.contains(&Action::DistMode(3)));
        match p.actions.iter().find(|a| matches!(a, Action::Input { .. })) {
            Some(Action::Input { dev, ch, .. }) => assert_eq!((*dev, *ch), (Some(0), Some(0))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn numbers_ranges_levels_and_averages_are_read() {
        let p = plan("frequency response with a sweep from 50 hz to 16 khz, 6 s long, -20 dbfs, 4 averages", &devs(), 0, false);
        assert!(p.actions.contains(&Action::Sweep { f0: Some(50.0), f1: Some(16000.0), secs: Some(6.0), level: Some(-20.0), averages: Some(4) }), "{:?}", p.actions);
        let q = plan("sweep 20 to 20 khz", &devs(), 0, false);
        assert!(q.actions.iter().any(|a| matches!(a, Action::Sweep { f0: Some(f), f1: Some(g), .. } if *f == 20.0 && *g == 20000.0)), "{:?}", q.actions);
        assert_eq!(words("mic1 20hz -12db 1.5khz"), vec!["mic", "1", "20", "hz", "-12", "db", "1.5", "khz"]);
    }

    #[test]
    fn other_tasks_pick_their_windows_and_the_stepped_modes() {
        assert!(windows(&plan("check THD and IMD of the line input", &devs(), 0, false)).contains(&"Distortion"));
        let p = plan("THD vs level on the line input", &devs(), 0, false);
        assert_eq!(p.runs, vec![Run::Stepped(1)]);
        match p.actions.iter().find(|a| matches!(a, Action::Input { .. })) {
            Some(Action::Input { dev, .. }) => assert_eq!(*dev, Some(4)),
            other => panic!("{other:?}"),
        }
        assert!(windows(&plan("measure the loudness of the music", &devs(), 0, false)).contains(&"Loudness"));
        assert!(windows(&plan("room acoustics rt60", &devs(), 0, false)).contains(&"Impulse"));
        assert!(windows(&plan("show the spectrum and spectrogram of channel 2", &devs(), 0, false)).contains(&"Spectro"));
        let g = plan("generate a 1 khz tone at -20 db and monitor it", &devs(), 0, false);
        assert!(g.actions.iter().any(|a| matches!(a, Action::Tone { freq: Some(f), level: Some(l), .. } if *f == 1000.0 && *l == -20.0)), "{:?}", g.actions);
        assert!(g.actions.contains(&Action::Monitor));
        assert!(g.runs.is_empty());
        let l = plan("use the loopback input for a sweep", &devs(), 0, false);
        match l.actions.iter().find(|a| matches!(a, Action::Input { .. })) {
            Some(Action::Input { dev, .. }) => assert_eq!(*dev, Some(2)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_example_button_makes_a_plan() {
        for (label, sentence) in EXAMPLES {
            let p = plan(sentence, &devs(), 1, false);
            assert!(!p.actions.is_empty(), "{label}: {sentence}");
            assert!(p.actions.iter().any(|a| matches!(a, Action::Windows(_))) || p.actions.contains(&Action::Monitor), "{label}");
        }
    }

    #[test]
    fn german_french_and_spanish_work_and_typos_are_fixed() {
        let p = plan("Frequenzgang, Impulsantwort und Verzerrung mit Sweep am Eingang Mikrofon 1 messen", &devs(), 1, false);
        let w = windows(&p);
        for n in ["Response", "Impulse", "Distortion"] {
            assert!(w.contains(&n), "{n} in {w:?}");
        }
        assert_eq!(p.runs, vec![Run::Sweep]);
        match p.actions.iter().find(|a| matches!(a, Action::Input { .. })) {
            Some(Action::Input { dev, ch, .. }) => assert_eq!((*dev, *ch), (Some(0), Some(0))),
            other => panic!("{other:?}"),
        }
        let q = plan("Sweep von 50 Hz bis 16 kHz, 6 Sekunden, -20 dBFS, 4 Mittelungen", &devs(), 0, false);
        assert!(q.actions.contains(&Action::Sweep { f0: Some(50.0), f1: Some(16000.0), secs: Some(6.0), level: Some(-20.0), averages: Some(4) }), "{:?}", q.actions);
        assert!(windows(&plan("mesurer la réponse en fréquence et la distorsion", &devs(), 0, false)).contains(&"Distortion"));
        assert!(windows(&plan("medir la respuesta en frecuencia", &devs(), 0, false)).contains(&"Response"));
        assert!(windows(&plan("Lautheit der Musik messen", &devs(), 0, false)).contains(&"Loudness"));
        // typos
        let t = plan("mesure the frequncy respnse and distortoin", &devs(), 0, false);
        let w = windows(&t);
        assert!(w.contains(&"Response") && w.contains(&"Distortion"), "{w:?}");
        assert!(matches!(correct("spektrogam").as_str(), "spectrogram" | "spektrogramm"));
        assert_eq!(correct("hello"), "hello");
        assert_eq!(correct("sweep"), "sweep");
    }

    #[test]
    fn a_from_that_is_not_a_range_does_not_hide_the_real_one() {
        let p = plan("frequency response from mic 1 with a sweep from 50 hz to 16 khz", &devs(), 0, false);
        assert!(p.actions.iter().any(|a| matches!(a, Action::Sweep { f0: Some(f), f1: Some(g), .. } if *f == 50.0 && *g == 16000.0)), "{:?}", p.actions);
    }

    #[test]
    fn the_word_frequency_alone_is_not_a_sweep_and_numbers_stay_with_their_words() {
        for s in ["generate a tone at frequency 440 hz", "set the generator frequency to 1 khz"] {
            let p = plan(s, &devs(), 0, false);
            assert!(p.runs.is_empty(), "{s}: {:?}", p.runs);
            assert!(windows(&p).contains(&"Generator") && !windows(&p).contains(&"Response"), "{s}");
        }
        assert!(windows(&plan("what is the frequency of this note", &devs(), 0, false)).contains(&"Peaks"));
        // the 2 of "mic 2" is the channel, the 4 the averages
        let p = plan("frequency response from 20 hz to 20 khz with 3 s sweep on mic 2 averaged 4 times", &devs(), 0, false);
        assert!(p.actions.iter().any(|a| matches!(a, Action::Sweep { averages: Some(4), secs: Some(s), .. } if *s == 3.0)), "{:?}", p.actions);
        assert!(windows(&plan("measure the noise floor", &devs(), 0, false)).contains(&"Measure"));
        assert!(windows(&plan("check the level of the left channel", &devs(), 0, false)).contains(&"Measure"));
    }

    #[test]
    fn random_text_never_panics() {
        let mut seed = 0x1234_5678_9abc_def1u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let pieces = ["from", "to", "50", "hz", "khz", "-20", "db", "sweep", "mic", "1", "ß", "é", "ü", "/", "1/3", ".", "-", "  ", "frequency", "response", "impulse", "distortion", "bis", "von", "average", "4", "s", "thd", "vs", "level", "tone", "99999999999", "-0", "0.0.0", "😀", "日本語", "x"];
        for _ in 0..3000 {
            let n = 1 + next() % 14;
            let s: Vec<&str> = (0..n).map(|_| pieces[(next() % pieces.len() as u64) as usize]).collect();
            let text = s.join(if next() % 2 == 0 { " " } else { "" });
            let r = std::panic::catch_unwind(|| plan(&text, &devs(), 1, false));
            assert!(r.is_ok(), "panicked on {text:?}");
        }
    }

    #[test]
    fn nonsense_and_the_routing_graph_are_handled() {
        let p = plan("make me a sandwich", &devs(), 0, false);
        assert!(p.actions.is_empty() && !p.notes.is_empty());
        assert!(plan("", &devs(), 0, false).actions.is_empty());
        // with the graph on the input is not switched, the Routing window opens
        let g = plan("frequency response on mic 2", &devs(), 0, true);
        assert!(!g.actions.iter().any(|a| matches!(a, Action::Input { .. })));
        assert!(windows(&g).contains(&"Routing"));
        assert!(g.notes.iter().any(|n| n.contains("Routing")));
        // an unknown device name keeps the current one and says so
        let u = plan("distortion on the zzzqqq input", &devs(), 1, false);
        assert!(u.notes.iter().any(|n| n.contains("No input device matches")), "{:?}", u.notes);
        // no devices at all does not panic
        let _ = plan("frequency response on mic1", &[], 0, false);
    }
}

#[cfg(test)]
mod probe {
    use super::*;
    #[test]
    #[ignore]
    fn probe_sentences() {
        let devs = vec!["Microphone (UMIK-1)".to_string(), "Speakers".to_string(), "Line (SSL 2+ USB)".to_string()];
        for s in [
            "generate a tone at frequency 440 hz",
            "set the generator frequency to 1 khz",
            "what is the frequency of this note",
            "tune my guitar",
            "check the level of the left channel",
            "measure the noise floor",
            "measure signal to noise ratio",
            "listen to the input",
            "show me the phase",
            "record 10 seconds from input 2",
            "sweep",
            "thd",
            "compare two files",
            "low frequency response of the subwoofer",
            "I want to see the spectrum and measure rt60",
            "measure distortion of the line input at 1 khz and -10 db",
            "frequency response from 20 hz to 20 khz with 3 s sweep on mic 2 averaged 4 times",
        ] {
            let p = plan(s, &devs, 1, false);
            println!("{s:70} -> {:?} | runs {:?} | {}", p.actions.iter().map(|a| match a { Action::Windows(w) => w.join("+"), Action::Input { label, ch, .. } => format!("in:{label}:{ch:?}"), other => format!("{other:?}") }).collect::<Vec<_>>(), p.runs, p.notes.len());
        }
    }
}
