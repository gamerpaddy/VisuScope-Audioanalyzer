//! Thiele-Small parameters of a loudspeaker driver from its measured impedance, the way the Dayton DATS does it: the free-air measurement
//! gives Re, Fs, Qms, Qes, Qts and Le; the compliance (Vas) and the moving mass (Mms) need one more piece of information, which can come
//! from a second measurement (the driver in a test box of known volume, or with a known mass added to the cone) or from a specification
//! (the sensitivity, or the moving mass of the cone).

use std::f32::consts::PI;

/// Air at 20 degrees (the values the formulas below use).
pub const RHO: f32 = 1.18;
pub const C_AIR: f32 = 345.0;

/// What the free-air measurement of the impedance gives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreeAir {
    pub re: f32,
    pub fs: f32,
    pub qms: f32,
    pub qes: f32,
    pub qts: f32,
    /// the peak of the impedance
    pub zmax: f32,
    /// the two frequencies where it is Re * sqrt(r0) (the width of the peak)
    pub f1: f32,
    pub f2: f32,
    /// the inductance of the voice coil at 1 kHz and at 10 kHz (henry; the reactance divided by the angular frequency)
    pub le_1k: f32,
    pub le_10k: f32,
}

/// The method that gives the compliance and the moving mass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VasMethod {
    /// the driver in a closed test box of this volume (litres): the resonance goes up
    TestBox(f32),
    /// a mass (grams) stuck to the cone: the resonance goes down
    AddedMass(f32),
    /// the sensitivity in dB at 1 W and 1 m from the data sheet
    Spl(f32),
    /// the moving mass of the cone without the air load (grams), from the data sheet or by weighing
    Mmd(f32),
}

/// The mechanical numbers that follow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mech {
    /// moving mass including the air load (kg)
    pub mms: f32,
    /// compliance (m/N)
    pub cms: f32,
    /// equivalent volume of air (m3)
    pub vas: f32,
    /// force factor (T m)
    pub bl: f32,
    /// mechanical resistance (kg/s)
    pub rms: f32,
    /// reference efficiency (fraction) and the sensitivity at 1 W / 1 m, half space (dB)
    pub eta0: f32,
    pub spl: f32,
}

/// Interpolate where `y` crosses `level` between two samples (linear in the log of the frequency).
fn cross(f: &[f32], y: &[f32], i0: usize, i1: usize, level: f32) -> f32 {
    let (a, b) = (y[i0] - level, y[i1] - level);
    let t = if (a - b).abs() < 1e-12 { 0.0 } else { a / (a - b) };
    (f[i0].ln() + t * (f[i1].ln() - f[i0].ln())).exp()
}

/// The free-air parameters from an impedance curve: frequencies `f` (rising), magnitude in ohm, phase in degrees (the leads already taken off).
pub fn free_air(f: &[f32], ohm: &[f32], phase_deg: &[f32]) -> Result<FreeAir, String> {
    let n = f.len();
    if n < 20 || ohm.len() != n || phase_deg.len() != n {
        return Err("no impedance curve".into());
    }
    // the resonance: the highest point below 3 kHz (a coil does not make a peak there), refined from the parabola through its neighbours
    let top = f.partition_point(|&x| x < 3000.0).clamp(3, n - 2);
    let ip = (1..top).max_by(|&a, &b| ohm[a].total_cmp(&ohm[b])).unwrap_or(1);
    if ip < 2 || ip + 2 >= n {
        return Err("no resonance peak in the measured range: measure from a lower frequency (a quarter of the resonance or lower)".into());
    }
    let (x0, x1, x2) = (f[ip - 1].ln(), f[ip].ln(), f[ip + 1].ln());
    let (y0, y1, y2) = (ohm[ip - 1], ohm[ip], ohm[ip + 1]);
    let d = (x1 - x0) * (x2 - x1) * (x2 - x0);
    let (fs, zmax) = {
        let a = (x2 * (y1 - y0) + x1 * (y0 - y2) + x0 * (y2 - y1)) / d;
        let b = (x2 * x2 * (y0 - y1) + x1 * x1 * (y2 - y0) + x0 * x0 * (y1 - y2)) / d;
        if a < -1e-12 {
            let xp = (-b / (2.0 * a)).clamp(x0, x2);
            let c = y1 - a * x1 * x1 - b * x1;
            (xp.exp(), (a * xp * xp + b * xp + c).max(y1))
        } else {
            (f[ip], y1)
        }
    };
    // Re: the resistance of the coil, the real part of the impedance towards 0 Hz. Below the resonance the mechanical part adds a real part
    // that grows like f^2 / (1 - (f / fs)^2)^2, so a straight line of the real part over that quantity, taken back to zero, gives Re.
    let idx: Vec<usize> = (0..n).filter(|&i| f[i] <= fs * 0.5).collect();
    if idx.len() < 4 || f[0] > fs * 0.5 {
        return Err("the sweep starts too close to the resonance: start it at a quarter of the resonance or lower".into());
    }
    let re_at = |i: usize| ohm[i] * phase_deg[i].to_radians().cos();
    let (mut sx, mut sy, mut sxx, mut sxy) = (0f64, 0f64, 0f64, 0f64);
    for &i in &idx {
        let r = (f[i] / fs) as f64;
        let x = (f[i] as f64).powi(2) / (1.0 - r * r).powi(2);
        let y = re_at(i) as f64;
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
    }
    let m = idx.len() as f64;
    let den = m * sxx - sx * sx;
    let re = if den.abs() > 1e-9 {
        let k = (m * sxy - sx * sy) / den;
        let b = (sy - k * sx) / m;
        // (a slope that is not positive means the model does not fit: the real part at the lowest frequency is the best there is)
        if k > 0.0 && b > 0.0 { b as f32 } else { re_at(0) }
    } else {
        re_at(0)
    };
    if re < 0.05 || zmax < re * 1.05 {
        return Err("the peak of the impedance is too small against the resistance: is this a driver in free air?".into());
    }
    let r0 = zmax / re;
    let level = (re * zmax).sqrt();
    // where the impedance is sqrt(Re * Zmax) on both sides of the peak
    let lo = (1..ip).rev().find(|&i| ohm[i - 1] <= level && ohm[i] > level).ok_or("the lower flank of the peak is not in the measured range")?;
    let f1 = cross(f, ohm, lo - 1, lo, level);
    let hi = (ip..n - 1).find(|&i| ohm[i] > level && ohm[i + 1] <= level).ok_or("the upper flank of the peak is not in the measured range")?;
    let f2 = cross(f, ohm, hi, hi + 1, level);
    let qms = fs * r0.sqrt() / (f2 - f1);
    let qes = qms / (r0 - 1.0);
    let qts = qms * qes / (qms + qes);
    // the inductance from the reactance
    let le = |at: f32| -> f32 {
        let k = f.partition_point(|&x| x < at).min(n - 1);
        ohm[k] * phase_deg[k].to_radians().sin() / (2.0 * PI * f[k])
    };
    Ok(FreeAir { re, fs, qms, qes, qts, zmax, f1, f2, le_1k: le(1000.0), le_10k: le(10_000.0) })
}

/// The area of the piston for an effective diameter (m).
pub fn piston_area(d_m: f32) -> f32 {
    PI * (d_m * 0.5) * (d_m * 0.5)
}

/// Mass of the air that moves with a piston of diameter `d_m` in free air (both sides), kg.
pub fn air_load(d_m: f32) -> f32 {
    let a = d_m * 0.5;
    16.0 / 3.0 * RHO * a * a * a
}

/// The mechanical numbers from the free-air parameters, the piston diameter (m) and one more piece of information: for the test box and the
/// added mass method the resonance of the second measurement (`fs2`).
pub fn mechanics(fa: &FreeAir, d_m: f32, method: VasMethod, fs2: Option<f32>) -> Result<Mech, String> {
    let sd = piston_area(d_m);
    if sd <= 0.0 {
        return Err("give the diameter of the piston".into());
    }
    let w = 2.0 * PI * fa.fs;
    let rc2 = RHO * C_AIR * C_AIR;
    let (mms, cms) = match method {
        VasMethod::AddedMass(g) => {
            let fs2 = fs2.ok_or("measure the driver with the mass on the cone first")?;
            if fs2 >= fa.fs * 0.999 {
                return Err("the resonance with the mass is not lower than without it".into());
            }
            let mms = g * 1e-3 / ((fa.fs / fs2).powi(2) - 1.0);
            (mms, 1.0 / (w * w * mms))
        }
        VasMethod::TestBox(l) => {
            let fs2 = fs2.ok_or("measure the driver in the box first")?;
            if fs2 <= fa.fs * 1.001 {
                return Err("the resonance in the box is not higher than in free air".into());
            }
            let vas = l * 1e-3 * ((fs2 / fa.fs).powi(2) - 1.0);
            let cms = vas / (rc2 * sd * sd);
            (1.0 / (w * w * cms), cms)
        }
        VasMethod::Spl(db) => {
            // eta0 = 4 pi^2 fs^3 Vas / (c^3 Qes) and SPL = 112.1 + 10 log eta0
            let eta0 = 10f32.powf((db - 112.1) / 10.0);
            let vas = eta0 * C_AIR.powi(3) * fa.qes / (4.0 * PI * PI * fa.fs.powi(3));
            let cms = vas / (rc2 * sd * sd);
            (1.0 / (w * w * cms), cms)
        }
        VasMethod::Mmd(g) => {
            let mms = g * 1e-3 + air_load(d_m);
            (mms, 1.0 / (w * w * mms))
        }
    };
    let vas = rc2 * sd * sd * cms;
    let bl = (w * mms * fa.re / fa.qes).sqrt();
    let rms = w * mms / fa.qms;
    let eta0 = 4.0 * PI * PI * fa.fs.powi(3) * vas / (C_AIR.powi(3) * fa.qes);
    Ok(Mech { mms, cms, vas, bl, rms, eta0, spl: 112.1 + 10.0 * eta0.max(1e-12).log10() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The impedance of a driver: the coil in series with a parallel resonance (the model the parameters come from).
    fn driver(re: f32, le: f32, fs: f32, qms: f32, qes: f32) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        // Rr / Re = r0 - 1 with r0 = 1 + Qms / Qes
        let rr = re * qms / qes;
        let f: Vec<f32> = (0..900).map(|i| 20.0 * 1000f32.powf(i as f32 / 899.0)).collect();
        let (mut ohm, mut ph) = (Vec::new(), Vec::new());
        for &x in &f {
            let w = 2.0 * PI * x;
            let w0 = 2.0 * PI * fs;
            let c = qms / (rr * w0);
            let l = 1.0 / (w0 * w0 * c);
            let y = (1.0 / rr, w * c - 1.0 / (w * l));
            let d = y.0 * y.0 + y.1 * y.1;
            let zm = (y.0 / d, -y.1 / d);
            let z = (re + zm.0, w * le + zm.1);
            ohm.push((z.0 * z.0 + z.1 * z.1).sqrt());
            ph.push(z.1.atan2(z.0).to_degrees());
        }
        (f, ohm, ph)
    }

    #[test]
    fn the_free_air_parameters_come_back_from_a_modelled_driver() {
        let (f, z, p) = driver(6.2, 0.4e-3, 62.0, 3.0, 0.66);
        let fa = free_air(&f, &z, &p).unwrap();
        assert!((fa.re - 6.2).abs() < 0.25, "Re {}", fa.re);
        assert!((fa.fs - 62.0).abs() < 0.6, "fs {}", fa.fs);
        assert!((fa.qms - 3.0).abs() < 0.2, "Qms {}", fa.qms);
        assert!((fa.qes - 0.66).abs() < 0.06, "Qes {}", fa.qes);
        assert!((fa.qts - 0.66 * 3.0 / 3.66).abs() < 0.05, "Qts {}", fa.qts);
        assert!((fa.le_1k - 0.4e-3).abs() < 0.1e-3, "Le {}", fa.le_1k);
        // a flat curve has no resonance
        let flat: Vec<f32> = f.iter().map(|_| 8.0).collect();
        assert!(free_air(&f, &flat, &p).is_err());
    }

    #[test]
    fn added_mass_test_box_spl_and_mass_agree_on_one_driver() {
        // a driver: 8 ohm, fs 50 Hz, 0.3 mm/N, 14 g, 12 cm piston
        let (d, mms, cms) = (0.12f32, 0.014f32, 0.0003f32);
        let fs = 1.0 / (2.0 * PI * (mms * cms).sqrt());
        let sd = piston_area(d);
        let vas = RHO * C_AIR * C_AIR * sd * sd * cms;
        let (f, z, p) = driver(6.0, 0.5e-3, fs, 4.0, 0.5);
        let fa = free_air(&f, &z, &p).unwrap();
        // added mass of 10 g: the resonance goes down
        let fs_m = 1.0 / (2.0 * PI * ((mms + 0.010) * cms).sqrt());
        let a = mechanics(&fa, d, VasMethod::AddedMass(10.0), Some(fs_m)).unwrap();
        assert!((a.mms - mms).abs() / mms < 0.03, "Mms {}", a.mms);
        assert!((a.vas - vas).abs() / vas < 0.06, "Vas {} vs {vas}", a.vas);
        // the test box: 20 litres: the resonance goes up
        let fs_b = fs * (1.0 + vas / 0.020).sqrt();
        let b = mechanics(&fa, d, VasMethod::TestBox(20.0), Some(fs_b)).unwrap();
        assert!((b.vas - vas).abs() / vas < 0.06, "box Vas {} vs {vas}", b.vas);
        // the specified mass: Mmd plus the air load
        let mmd = (mms - air_load(d)) * 1000.0;
        let c = mechanics(&fa, d, VasMethod::Mmd(mmd), None).unwrap();
        assert!((c.vas - vas).abs() / vas < 0.06, "Mmd Vas {} vs {vas}", c.vas);
        // the sensitivity method: the SPL that goes with this driver gives its Vas back
        let spl = 112.1 + 10.0 * (4.0 * PI * PI * fa.fs.powi(3) * vas / (C_AIR.powi(3) * fa.qes)).log10();
        let s = mechanics(&fa, d, VasMethod::Spl(spl), None).unwrap();
        assert!((s.vas - vas).abs() / vas < 0.01, "SPL Vas {} vs {vas}", s.vas);
        // nonsense is refused
        assert!(mechanics(&fa, d, VasMethod::AddedMass(10.0), Some(fa.fs * 1.1)).is_err());
        assert!(mechanics(&fa, d, VasMethod::TestBox(20.0), None).is_err());
        // Bl is in a believable range for this driver
        assert!(a.bl > 1.0 && a.bl < 30.0, "Bl {}", a.bl);
    }
}
