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
