use eframe::egui::Color32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmap {
    Inferno,
    Magma,
    Viridis,
    Turbo,
    Gray,
}

type Poly = [[f32; 3]; 7];

const INFERNO: Poly = [
    [0.000_218_940_37, 0.001_651_004_7, -0.019_480_899],
    [0.106_513_42, 0.563_956_44, 3.932_712_3],
    [11.602_493, -3.972_854, -15.942_394],
    [-41.703_995, 17.436_4, 44.354_145],
    [77.162_93, -33.402_36, -81.807_31],
    [-71.319_43, 32.626_064, 73.209_52],
    [25.131_126, -12.242_669, -23.070_326],
];
const MAGMA: Poly = [
    [-0.002_136_485, -0.000_749_655_07, -0.005_386_127_6],
    [0.251_660_56, 0.677_523_26, 2.494_026_7],
    [8.353_717, -3.577_719_4, 0.314_467_9],
    [-27.668_734, 14.264_73, -13.649_213],
    [52.176_14, -27.943_605, 12.944_169],
    [-50.768_524, 29.046_583, 4.234_153],
    [18.655_704, -11.489_774, -5.601_961_6],
];
const VIRIDIS: Poly = [
    [0.277_727_34, 0.005_407_344_5, 0.334_099_8],
    [0.105_093_04, 1.404_613_5, 1.384_590_1],
    [-0.330_861_84, 0.214_847_56, 0.095_095_165],
    [-4.634_230_6, -5.799_101, -19.332_441],
    [6.228_27, 14.179_934, 56.690_55],
    [4.776_385, -13.745_146, -65.353_035],
    [-5.435_456, 4.645_852_6, 26.312_435],
];

fn poly(c: &Poly, t: f32) -> [f32; 3] {
    let mut out = [0.0; 3];
    for ch in 0..3 {
        let mut v = c[6][ch];
        for k in (0..6).rev() {
            v = v * t + c[k][ch];
        }
        out[ch] = v;
    }
    out
}

fn to_c32(rgb: [f32; 3]) -> Color32 {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Color32::from_rgb(f(rgb[0]), f(rgb[1]), f(rgb[2]))
}

impl Cmap {
    pub const ALL: [Cmap; 5] = [Cmap::Inferno, Cmap::Magma, Cmap::Viridis, Cmap::Turbo, Cmap::Gray];

    pub fn name(self) -> &'static str {
        match self {
            Cmap::Inferno => "Inferno",
            Cmap::Magma => "Magma",
            Cmap::Viridis => "Viridis",
            Cmap::Turbo => "Turbo",
            Cmap::Gray => "Grayscale",
        }
    }

    pub fn at(self, t: f32) -> Color32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Cmap::Inferno => to_c32(poly(&INFERNO, t)),
            Cmap::Magma => to_c32(poly(&MAGMA, t)),
            Cmap::Viridis => to_c32(poly(&VIRIDIS, t)),
            Cmap::Turbo => {
                let r = 0.13572138 + t * (4.615_392_7 + t * (-42.660_324 + t * (132.131_09 + t * (-152.942_4 + t * 59.286_38))));
                let g = 0.09140261 + t * (2.194_188_4 + t * (4.842_966_6 + t * (-14.185_034 + t * (4.277_298_5 + t * 2.829_566))));
                let b = 0.106_673_3 + t * (12.641_946 + t * (-60.582_047 + t * (110.362_77 + t * (-89.903_11 + t * 27.348_25))));
                to_c32([r, g, b])
            }
            Cmap::Gray => to_c32([t, t, t]),
        }
    }

    pub fn lut(self) -> [Color32; 256] {
        let mut l = [Color32::BLACK; 256];
        for (i, c) in l.iter_mut().enumerate() {
            *c = self.at(i as f32 / 255.0);
        }
        l
    }
}
