#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod cal;
mod colormap;
mod dsp;
mod eq;
mod gen_ui;
mod layouts;
mod report;
mod log;
mod options;
mod scope;
mod shared;
mod sidebar;
mod stereo;
mod tf;
mod tonegen;
mod toolhost;
mod tools;
mod view3d;
mod views;
mod windows;

/// "1.1.0 (abc1234)"; a trailing + on the commit marks uncommitted changes.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("VS_GIT"), ")");
/// Version plus the time this binary was compiled.
pub const VERSION_FULL: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("VS_GIT"), "), built ", env!("VS_BUILD"));

/// Set when the window was created at its final size, so the app need not resize it later.
pub static PRESIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Primary screen size in logical points (the process is DPI-unaware until the window system starts).
#[cfg(windows)]
fn primary_screen_size() -> Option<(f32, f32)> {
    unsafe extern "system" {
        fn GetSystemMetrics(index: i32) -> i32;
    }
    // SM_CXSCREEN = 0, SM_CYSCREEN = 1
    let (w, h) = unsafe { (GetSystemMetrics(0), GetSystemMetrics(1)) };
    (w >= 640 && h >= 400).then_some((w as f32, h as f32))
}

#[cfg(not(windows))]
fn primary_screen_size() -> Option<(f32, f32)> {
    None
}

fn main() -> eframe::Result {
    log::init();
    // Release builds abort on panic; keep the message in a file so it can be reported.
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{info}\n{}", std::backtrace::Backtrace::force_capture());
        eprintln!("{msg}");
        log::log(&format!("PANIC: {msg}"));
        let _ = std::fs::write(log::path().with_file_name("visuscope_panic.log"), msg);
    }));
    let mut viewport = eframe::egui::ViewportBuilder::default().with_min_inner_size([800.0, 500.0]).with_title(format!("VisuScope {VERSION} - audio analyzer"));
    viewport = match primary_screen_size() {
        Some((sw, sh)) => {
            // 75% of the screen, centred, created that way from the start (no resize flash)
            let (mut w, mut h) = (sw * 0.75, sh * 0.75);
            // test hook for reproducible screenshots: VISUSCOPE_SIZE=1920x1080
            if let Some((ew, eh)) = std::env::var("VISUSCOPE_SIZE").ok().and_then(|v| v.split_once('x').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))) {
                (w, h) = (ew.min(sw), eh.min(sh));
            }
            PRESIZED.store(true, std::sync::atomic::Ordering::Relaxed);
            viewport.with_inner_size([w, h]).with_position([(sw - w) * 0.5, (sh - h) * 0.5 - 20.0])
        }
        // otherwise the app resizes itself to 75% of the monitor on the first frames
        None => viewport.with_inner_size([1280.0, 800.0]),
    };
    let opts = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("VisuScope", opts, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
