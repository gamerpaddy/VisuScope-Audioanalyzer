//! Measurement report: tools describe what they have as sections (text, tables, plots) and this module
//! renders them as one self-contained HTML page with inline SVG charts. Open it in a browser and
//! print to PDF if a PDF is needed.

use std::fmt::Write;

#[derive(Clone)]
pub struct Series {
    pub name: String,
    pub color: [u8; 3],
    pub pts: Vec<(f32, f32)>,
}

#[derive(Clone)]
pub struct PlotSpec {
    pub title: String,
    pub xlog: bool,
    pub xlabel: String,
    pub ylabel: String,
    pub x: (f32, f32),
    pub y: (f32, f32),
    pub series: Vec<Series>,
}

#[derive(Clone)]
pub enum Block {
    Text(String),
    Table { head: Vec<String>, rows: Vec<Vec<String>> },
    Plot(PlotSpec),
    /// pass / fail banner
    Verdict(bool, String),
}

#[derive(Clone)]
pub struct Section {
    /// matches the include list of the Report window
    pub id: &'static str,
    pub title: String,
    pub blocks: Vec<Block>,
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn nice_step(range: f32, target: f32) -> f32 {
    crate::views::nice_step(range, target)
}

/// Keep at most `n` points by taking every k-th (the curves are already smooth or on a grid).
pub fn thin(pts: Vec<(f32, f32)>, n: usize) -> Vec<(f32, f32)> {
    if pts.len() <= n {
        return pts;
    }
    let k = pts.len().div_ceil(n);
    pts.into_iter().step_by(k).collect()
}

/// Axis range covering the finite y values of all series, rounded outwards to multiples of `step`.
pub fn auto_range(series: &[Series], step: f32, min_span: f32) -> (f32, f32) {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for s in series {
        for &(_, y) in &s.pts {
            if y.is_finite() {
                lo = lo.min(y);
                hi = hi.max(y);
            }
        }
    }
    if lo > hi {
        return (-step, step);
    }
    let (mut a, mut b) = ((lo / step).floor() * step, (hi / step).ceil() * step);
    if b - a < min_span {
        b = a + min_span;
    }
    if a == b {
        a -= step;
    }
    (a, b)
}

/// Series colours that stay readable on white.
pub const COLORS: [[u8; 3]; 8] = [[43, 108, 176], [214, 99, 18], [34, 139, 70], [150, 70, 180], [200, 40, 90], [90, 90, 90], [0, 150, 170], [160, 120, 0]];

pub fn svg_plot(p: &PlotSpec) -> String {
    let (w, h) = (760.0f32, 300.0f32);
    let (l, r, t, b) = (58.0f32, 14.0f32, 26.0f32, 36.0f32);
    let (pw, ph) = (w - l - r, h - t - b);
    let (x0, x1) = (p.x.0, p.x.1.max(p.x.0 * 1.0001 + 1e-6));
    let (y0, y1) = (p.y.0, p.y.1.max(p.y.0 + 1e-6));
    let tx = |x: f32| -> f32 {
        let (a, bb, v) = if p.xlog { (x0.max(1e-6).ln(), x1.max(1e-6).ln(), x.max(1e-6).ln()) } else { (x0, x1, x) };
        l + (v - a) / (bb - a).max(1e-9) * pw
    };
    let ty = |y: f32| t + ph - (y - y0) / (y1 - y0) * ph;
    let mut s = format!("<svg viewBox=\"0 0 {w} {h}\" width=\"100%\" xmlns=\"http://www.w3.org/2000/svg\" font-family=\"Segoe UI, Arial, sans-serif\" font-size=\"10\">");
    let _ = write!(s, "<rect x=\"{l}\" y=\"{t}\" width=\"{pw}\" height=\"{ph}\" fill=\"#fff\" stroke=\"#999\"/>");
    // horizontal grid
    let ystep = nice_step(y1 - y0, 7.0);
    let mut v = (y0 / ystep).ceil() * ystep;
    while v <= y1 + 1e-4 {
        let y = ty(v);
        let _ = write!(s, "<line x1=\"{l}\" x2=\"{}\" y1=\"{y:.1}\" y2=\"{y:.1}\" stroke=\"#ddd\"/><text x=\"{}\" y=\"{:.1}\" text-anchor=\"end\" fill=\"#555\">{}</text>", l + pw, l - 4.0, y + 3.0, trim_num(v));
        v += ystep;
    }
    // vertical grid
    let mut xs: Vec<(f32, bool)> = Vec::new();
    if p.xlog {
        let (e0, e1) = (x0.max(1e-6).log10().floor() as i32, x1.log10().ceil() as i32);
        for e in e0..=e1 {
            for m in 1..10 {
                let f = m as f32 * 10f32.powi(e);
                if f >= x0 * 0.9999 && f <= x1 * 1.0001 {
                    xs.push((f, m == 1 || m == 2 || m == 5));
                }
            }
        }
    } else {
        let step = nice_step(x1 - x0, 8.0);
        let mut x = (x0 / step).ceil() * step;
        while x <= x1 + 1e-4 {
            xs.push((x, true));
            x += step;
        }
    }
    for (x, label) in xs {
        let px = tx(x);
        let major = !p.xlog || x.log10().fract().abs() < 1e-4 || (x / 10f32.powf(x.log10().floor()) - 1.0).abs() < 1e-3;
        let _ = write!(s, "<line y1=\"{t}\" y2=\"{}\" x1=\"{px:.1}\" x2=\"{px:.1}\" stroke=\"{}\"/>", t + ph, if major { "#ccc" } else { "#eee" });
        if label {
            let txt = if p.xlog { short_hz(x) } else { trim_num(x) };
            let _ = write!(s, "<text x=\"{px:.1}\" y=\"{}\" text-anchor=\"middle\" fill=\"#555\">{txt}</text>", t + ph + 13.0);
        }
    }
    // curves
    for se in &p.series {
        let col = format!("rgb({},{},{})", se.color[0], se.color[1], se.color[2]);
        let mut seg = String::new();
        let flush = |seg: &mut String, out: &mut String| {
            if seg.matches(' ').count() >= 2 {
                let _ = write!(out, "<polyline fill=\"none\" stroke=\"{col}\" stroke-width=\"1.4\" points=\"{}\"/>", seg.trim());
            }
            seg.clear();
        };
        for &(x, y) in &se.pts {
            if !x.is_finite() || !y.is_finite() {
                flush(&mut seg, &mut s);
                continue;
            }
            let _ = write!(seg, "{:.1},{:.1} ", tx(x), ty(y).clamp(t - 4.0, t + ph + 4.0));
        }
        flush(&mut seg, &mut s);
    }
    let _ = write!(s, "<text x=\"{}\" y=\"14\" font-size=\"12\" font-weight=\"600\" fill=\"#222\">{}</text>", l, esc(&p.title));
    let _ = write!(s, "<text x=\"{}\" y=\"{}\" text-anchor=\"end\" fill=\"#555\">{}</text>", l + pw, h - 4.0, esc(&p.xlabel));
    let _ = write!(s, "<text x=\"4\" y=\"{}\" fill=\"#555\">{}</text>", t + 8.0, esc(&p.ylabel));
    // legend
    let mut lx = l + 6.0;
    for se in &p.series {
        if se.name.is_empty() {
            continue;
        }
        let _ = write!(s, "<rect x=\"{lx}\" y=\"{}\" width=\"10\" height=\"3\" fill=\"rgb({},{},{})\"/><text x=\"{}\" y=\"{}\" fill=\"#333\">{}</text>", t + 8.0, se.color[0], se.color[1], se.color[2], lx + 14.0, t + 12.0, esc(&se.name));
        lx += 24.0 + se.name.chars().count() as f32 * 5.6;
    }
    s.push_str("</svg>");
    s
}

fn trim_num(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn short_hz(f: f32) -> String {
    if f >= 1000.0 {
        format!("{}k", trim_num(f / 1000.0))
    } else {
        trim_num(f)
    }
}

pub fn render_html(title: &str, notes: &str, meta: &[(String, String)], sections: &[Section]) -> String {
    let mut s = String::new();
    s += "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"color-scheme\" content=\"light\">";
    let _ = write!(s, "<title>{}</title>", esc(title));
    s += "<style>html{background:#fff;color-scheme:light}body{background:#fff;font:14px/1.45 'Segoe UI',Arial,sans-serif;color:#1d2229;max-width:880px;margin:24px auto;padding:0 18px}h1{font-size:24px;margin:0 0 4px}h2{font-size:17px;margin:28px 0 8px;padding-bottom:4px;border-bottom:2px solid #2b6cb0}\
table{border-collapse:collapse;margin:8px 0;font-size:13px}th,td{padding:3px 12px 3px 0;text-align:left;border-bottom:1px solid #e3e6ea}th{color:#555;font-weight:600}\
.meta td:first-child{color:#555}.notes{white-space:pre-wrap;background:#f5f7fa;border-left:3px solid #2b6cb0;padding:8px 12px;margin:12px 0}\
.pass,.fail{display:inline-block;padding:3px 12px;border-radius:4px;font-weight:700;margin:6px 0}.pass{background:#d7f5df;color:#14632b}.fail{background:#fde0dc;color:#9c2315}\
svg{margin:8px 0;page-break-inside:avoid}section{page-break-inside:auto}p.t{margin:6px 0;color:#333}@media print{body{margin:0;max-width:none}h2{page-break-after:avoid}}</style></head><body>";
    let _ = write!(s, "<h1>{}</h1>", esc(title));
    s += "<table class=\"meta\">";
    for (k, v) in meta {
        let _ = write!(s, "<tr><td>{}</td><td>{}</td></tr>", esc(k), esc(v));
    }
    s += "</table>";
    if !notes.trim().is_empty() {
        let _ = write!(s, "<div class=\"notes\">{}</div>", esc(notes.trim()));
    }
    for sec in sections {
        let _ = write!(s, "<section><h2>{}</h2>", esc(&sec.title));
        for b in &sec.blocks {
            match b {
                Block::Text(t) => {
                    let _ = write!(s, "<p class=\"t\">{}</p>", esc(t).replace('\n', "<br>"));
                }
                Block::Table { head, rows } => {
                    s += "<table>";
                    if !head.is_empty() {
                        s += "<tr>";
                        for h in head {
                            let _ = write!(s, "<th>{}</th>", esc(h));
                        }
                        s += "</tr>";
                    }
                    for r in rows {
                        s += "<tr>";
                        for c in r {
                            let _ = write!(s, "<td>{}</td>", esc(c));
                        }
                        s += "</tr>";
                    }
                    s += "</table>";
                }
                Block::Plot(p) => s += &svg_plot(p),
                Block::Verdict(ok, t) => {
                    let _ = write!(s, "<div class=\"{}\">{}</div>", if *ok { "pass" } else { "fail" }, esc(t));
                }
            }
        }
        s += "</section>";
    }
    s += "<p style=\"margin-top:32px;color:#888;font-size:12px\">Created with VisuScope</p></body></html>";
    s
}

/// REW / FRD style text export: frequency, magnitude (dB), phase (degrees).
pub fn frd_text(f: &[f32], mag: &[f32], phase: &[f32], rew_header: bool) -> String {
    let mut s = String::new();
    if rew_header {
        s += "* Measurement data exported by VisuScope\n* Freq(Hz), SPL(dB), Phase(degrees)\n";
    }
    for i in 0..f.len() {
        if !mag[i].is_finite() {
            continue;
        }
        let ph = phase.get(i).copied().filter(|p| p.is_finite()).unwrap_or(0.0);
        if rew_header {
            s += &format!("{:.5}, {:.3}, {:.3}\n", f[i], mag[i], ph);
        } else {
            s += &format!("{:.5} {:.3} {:.3}\n", f[i], mag[i], ph);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_plot_and_escapes() {
        let p = PlotSpec {
            title: "A & B".into(),
            xlog: true,
            xlabel: "Hz".into(),
            ylabel: "dB".into(),
            x: (20.0, 20000.0),
            y: (-20.0, 20.0),
            series: vec![Series { name: "x<y".into(), color: [1, 2, 3], pts: vec![(20.0, 0.0), (100.0, f32::NAN), (1000.0, 5.0), (2000.0, 6.0), (20000.0, -3.0)] }],
        };
        let html = render_html("T", "note <b>", &[("k".into(), "v".into())], &[Section { id: "x", title: "S".into(), blocks: vec![Block::Plot(p), Block::Verdict(true, "PASS".into()), Block::Table { head: vec!["a".into()], rows: vec![vec!["1".into()]] }] }]);
        assert!(html.contains("A &amp; B") && html.contains("x&lt;y") && html.contains("note &lt;b&gt;"));
        assert!(html.matches("<polyline").count() == 1, "NaN splits the line, the 1-point piece is dropped");
        assert!(html.contains("class=\"pass\"") && html.contains("</html>"));
        assert!(!html.contains("NaN"));
        let t = frd_text(&[100.0, 200.0], &[1.0, f32::NAN], &[10.0, 20.0], false);
        assert_eq!(t.lines().count(), 1);
    }
}
