//! Voltage calibration: maps digital full scale (peak 1.0) to volts.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Dbfs,
    Dbv,
    Dbu,
}

impl Unit {
    pub const ALL: [Unit; 3] = [Unit::Dbfs, Unit::Dbv, Unit::Dbu];

    pub fn name(self) -> &'static str {
        match self {
            Unit::Dbfs => "dBFS",
            Unit::Dbv => "dBV",
            Unit::Dbu => "dBu",
        }
    }

    pub fn from_index(i: u8) -> Unit {
        match i {
            1 => Unit::Dbv,
            2 => Unit::Dbu,
            _ => Unit::Dbfs,
        }
    }

    pub fn index(self) -> u8 {
        match self {
            Unit::Dbfs => 0,
            Unit::Dbv => 1,
            Unit::Dbu => 2,
        }
    }
}

pub const DBU_REF_V: f32 = 0.774_596_7;

pub struct Calib {
    /// Peak volts at digital full scale (|x| = 1.0), per channel.
    pub vpk: [f32; 2],
    pub on: bool,
    /// Reference RMS voltage applied to the input when calibrating from a measurement.
    pub ref_vrms: f32,
    /// 0 = tone fundamental, 1 = broadband RMS
    pub method: u8,
    /// 0 = Vpk, 1 = Vrms, 2 = dBV, 3 = dBu
    pub entry_unit: u8,
    pub entry_val: f32,
}

impl Calib {
    pub fn new() -> Self {
        Self { vpk: [1.0, 1.0], on: false, ref_vrms: 1.0, method: 0, entry_unit: 1, entry_val: 1.0 }
    }

    /// Display units per digital unit.
    pub fn scale(&self, ch: usize) -> f32 {
        if self.on { self.vpk[ch] } else { 1.0 }
    }

    pub fn entry_to_vpk(&self) -> f32 {
        let v = self.entry_val;
        let s2 = std::f32::consts::SQRT_2;
        match self.entry_unit {
            0 => v,
            1 => v * s2,
            2 => s2 * 10f32.powf(v / 20.0),
            _ => s2 * DBU_REF_V * 10f32.powf(v / 20.0),
        }
    }
}

pub fn fmt_volts(v: f32) -> String {
    let a = v.abs();
    if a >= 1.0 {
        format!("{v:.3} V")
    } else if a >= 1e-3 {
        format!("{:.2} mV", v * 1e3)
    } else {
        format!("{:.1} \u{b5}V", v * 1e6)
    }
}

pub fn fmt_time(t: f32) -> String {
    let a = t.abs();
    if a >= 1.0 {
        format!("{t:.3} s")
    } else if a >= 1e-3 {
        format!("{:.3} ms", t * 1e3)
    } else {
        format!("{:.2} \u{b5}s", t * 1e6)
    }
}

/// Microphone calibration curve from a `.txt` / `.cal` file: header lines (quoted or starting with `*`)
/// followed by `frequency  dB` pairs (the mic's deviation from flat; the display subtracts it).
#[derive(Clone)]
pub struct MicCurve {
    pub name: String,
    pub f: Vec<f32>,
    pub db: Vec<f32>,
    /// `Sens Factor` of a miniDSP / UMIK style header
    pub sens: Option<f32>,
    pub again: Option<f32>,
}

fn header_value(line: &str, key: &str) -> Option<f32> {
    let i = line.find(key)?;
    let rest = line[i + key.len()..].trim_start_matches([' ', '=', ':']);
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

pub fn parse_mic_cal(name: &str, text: &str) -> Option<MicCurve> {
    let (mut f, mut db) = (Vec::new(), Vec::new());
    let (mut sens, mut again) = (None, None);
    for line in text.lines() {
        let l = line.trim().trim_start_matches('\u{feff}');
        if l.is_empty() {
            continue;
        }
        if l.starts_with(['"', '*', '#', ';']) {
            sens = sens.or_else(|| header_value(l, "Sens Factor"));
            again = again.or_else(|| header_value(l, "AGain"));
            continue;
        }
        let mut it = l.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|s| !s.is_empty());
        let (Some(a), Some(b)) = (it.next(), it.next()) else { continue };
        if let (Ok(a), Ok(b)) = (a.parse::<f32>(), b.parse::<f32>()) {
            if a > 0.0 && a.is_finite() && b.is_finite() && f.last().map_or(true, |&p| a > p) {
                f.push(a);
                db.push(b);
            }
        }
    }
    (f.len() >= 2).then(|| MicCurve { name: name.to_string(), f, db, sens, again })
}

/// Correction (dB) at `freq` by interpolation on a log frequency axis, held flat outside the file's range.
pub fn mic_db_at(f: &[f32], db: &[f32], freq: f32) -> f32 {
    if f.is_empty() {
        return 0.0;
    }
    if freq <= f[0] {
        return db[0];
    }
    if freq >= *f.last().unwrap() {
        return *db.last().unwrap();
    }
    let j = f.partition_point(|&x| x < freq).clamp(1, f.len() - 1);
    let t = (freq.ln() - f[j - 1].ln()) / (f[j].ln() - f[j - 1].ln()).max(1e-9);
    db[j - 1] + t * (db[j] - db[j - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_umik_file() {
        let t = "\"Sens Factor =3.8346dB, AGain =18dB, SERNO: 7070781\"\n10.054\t-5.5826\n10.179\t-5.4112\n20\t0.0\n";
        let c = parse_mic_cal("x", t).unwrap();
        assert_eq!(c.f.len(), 3);
        assert_eq!(c.sens, Some(3.8346));
        assert_eq!(c.again, Some(18.0));
        assert!((mic_db_at(&c.f, &c.db, 10.054) + 5.5826).abs() < 1e-4);
        assert!(mic_db_at(&c.f, &c.db, 5.0) < -5.5);
    }
}
