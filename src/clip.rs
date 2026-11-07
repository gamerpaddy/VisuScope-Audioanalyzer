//! Text to the Windows clipboard (the egui one needs a feature this build leaves out).

#[cfg(windows)]
pub fn copy(text: &str) -> bool {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_UNICODETEXT: u32 = 13;
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
        let mut ok = false;
        if !h.is_null() {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                GlobalUnlock(h);
                ok = !SetClipboardData(CF_UNICODETEXT, h as _).is_null();
            }
        }
        CloseClipboard();
        ok
    }
}

#[cfg(not(windows))]
pub fn copy(_text: &str) -> bool {
    false
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    #[ignore]
    fn text_reaches_the_clipboard() {
        assert!(super::copy("VisuScope clip test äö µH\nline 2"));
    }
}
