//! Commands to the outside over a serial port (COM): a turntable, relays, anything an Arduino or another controller can do.
//!
//! The program only writes text. A command is a line of text that the user defines; `\n`, `\r` and `\t` stand for the control
//! characters, `\xNN` for any byte, and `{angle}` (or another name the sender gives) is replaced by a number. Commands are sent
//! by a thread of their own, so a slow or missing device never holds the program up. The port stays open between commands:
//! most boards restart when the port is opened, which is why the first command waits `boot_ms` after opening.
//!
//! The set-up is kept in `serial.txt` next to the settings.

use parking_lot::Mutex;
use std::io::Write;
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

pub const BAUDS: [u32; 8] = [9600, 19200, 38400, 57600, 115200, 230400, 250000, 500000];
pub const EOL: [(&str, &str); 4] = [("none", ""), ("LF", "\n"), ("CR LF", "\r\n"), ("CR", "\r")];

#[derive(Clone, PartialEq)]
pub struct Cfg {
    /// "COM3"; empty: nothing is sent
    pub port: String,
    pub baud: u32,
    /// index into `EOL`: what is added to every command
    pub eol: usize,
    /// wait after opening the port before the first command (a board that restarts needs about 2 s)
    pub boot_ms: u32,
    /// the user's commands: (name, text)
    pub cmds: Vec<(String, String)>,
}

impl Default for Cfg {
    fn default() -> Self {
        Self { port: String::new(), baud: 115200, eol: 1, boot_ms: 2000, cmds: vec![("Relay on".into(), "R1".into()), ("Relay off".into(), "R0".into())] }
    }
}

static CFG: Mutex<Option<Cfg>> = Mutex::new(None);
static STATUS: Mutex<String> = Mutex::new(String::new());
static TX: OnceLock<Sender<Vec<u8>>> = OnceLock::new();

fn file() -> std::path::PathBuf {
    crate::app::settings_path().with_file_name("serial.txt")
}

pub fn to_text(c: &Cfg) -> String {
    let mut s = format!("port={}\nbaud={}\neol={}\nboot_ms={}\n", c.port, c.baud, c.eol, c.boot_ms);
    for (n, t) in &c.cmds {
        s += &format!("cmd={}\t{}\n", n.replace(['\t', '\n', '\r'], " "), t.replace(['\n', '\r'], " "));
    }
    s
}

pub fn from_text(text: &str) -> Cfg {
    let mut c = Cfg { cmds: Vec::new(), ..Default::default() };
    for l in text.lines() {
        match l.split_once('=') {
            Some(("port", v)) if v.len() <= 16 && v.chars().all(|ch| ch.is_ascii_alphanumeric()) => c.port = v.to_string(),
            Some(("baud", v)) => c.baud = v.parse().ok().filter(|b| (300..=4_000_000).contains(b)).unwrap_or(115200),
            Some(("eol", v)) => c.eol = v.parse::<usize>().unwrap_or(1).min(EOL.len() - 1),
            Some(("boot_ms", v)) => c.boot_ms = v.parse::<u32>().unwrap_or(2000).min(20000),
            Some(("cmd", v)) => {
                if let Some((n, t)) = v.split_once('\t')
                    && c.cmds.len() < 64 {
                        c.cmds.push((n.to_string(), t.to_string()));
                    }
            }
            _ => {}
        }
    }
    c
}

/// The set-up (read from the file the first time).
pub fn cfg() -> Cfg {
    CFG.lock().get_or_insert_with(|| std::fs::read_to_string(file()).map(|t| from_text(&t)).unwrap_or_default()).clone()
}

pub fn set_cfg(c: Cfg) {
    let mut g = CFG.lock();
    if g.as_ref() != Some(&c) {
        let _ = crate::fsutil::atomic_write(file(), to_text(&c));
        *g = Some(c);
    }
}

pub fn status() -> String {
    STATUS.lock().clone()
}

/// The text of a command as the bytes that go out: escapes resolved, `{name}` replaced by its value.
pub fn expand(text: &str, vars: &[(&str, f32)]) -> Vec<u8> {
    let mut t = text.to_string();
    for (k, v) in vars {
        // a whole number goes out without a decimal point
        let num = if v.fract().abs() < 1e-4 { format!("{v:.0}") } else { format!("{v:.2}") };
        t = t.replace(&format!("{{{k}}}"), &num);
    }
    let b = t.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            let (c, used) = match b[i + 1] {
                b'n' => (Some(b'\n'), 2),
                b'r' => (Some(b'\r'), 2),
                b't' => (Some(b'\t'), 2),
                b'\\' => (Some(b'\\'), 2),
                b'x' if i + 3 < b.len() => (std::str::from_utf8(&b[i + 2..i + 4]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()), 4),
                _ => (None, 0),
            };
            if let Some(c) = c {
                out.push(c);
                i += used;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Send a command (with the line end of the set-up). Returns at once; `status()` tells how it went.
pub fn send(text: &str, vars: &[(&str, f32)]) {
    let c = cfg();
    if c.port.is_empty() {
        *STATUS.lock() = "no serial port chosen (Options)".into();
        return;
    }
    let mut bytes = expand(text, vars);
    bytes.extend_from_slice(EOL[c.eol.min(EOL.len() - 1)].1.as_bytes());
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("serial".into())
            .spawn(move || {
                let mut open: Option<(String, u32, std::fs::File)> = None;
                while let Ok(bytes) = rx.recv() {
                    let c = cfg();
                    if open.as_ref().is_some_and(|o| o.0 != c.port || o.1 != c.baud) {
                        open = None;
                    }
                    if open.is_none() {
                        match open_port(&c.port, c.baud) {
                            Ok(f) => {
                                *STATUS.lock() = format!("{} open, waiting for the device...", c.port);
                                std::thread::sleep(std::time::Duration::from_millis(c.boot_ms as u64));
                                open = Some((c.port.clone(), c.baud, f));
                            }
                            Err(e) => {
                                *STATUS.lock() = format!("{}: {e}", c.port);
                                continue;
                            }
                        }
                    }
                    let Some((_, _, f)) = open.as_mut() else { continue };
                    let shown = String::from_utf8_lossy(&bytes).trim_end().to_string();
                    match f.write_all(&bytes).and_then(|_| f.flush()) {
                        Ok(()) => *STATUS.lock() = format!("sent: {shown}"),
                        Err(e) => {
                            // the device was unplugged: open it again for the next command
                            *STATUS.lock() = format!("{}: {e}", c.port);
                            open = None;
                        }
                    }
                }
            })
            .ok();
        tx
    });
    let _ = tx.send(bytes);
}

/// Send the user's command of this name.
pub fn send_named(name: &str) {
    match cfg().cmds.iter().find(|c| c.0 == name) {
        Some((_, t)) => send(t, &[]),
        None => *STATUS.lock() = format!("no serial command named {name}"),
    }
}

/// Open the port for 8 data bits, no parity, one stop bit, with DTR and RTS set and a write that gives up after a second.
fn open_port(port: &str, baud: u32) -> Result<std::fs::File, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Devices::Communication::{GetCommState, SetCommState, SetCommTimeouts, COMMTIMEOUTS, DCB};
    let f = std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\{port}")).map_err(|e| e.to_string())?;
    // SAFETY: the handle is the open port; the structures are plain data of the sizes the calls expect
    unsafe {
        let h = f.as_raw_handle() as _;
        let mut dcb: DCB = std::mem::zeroed();
        dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
        if GetCommState(h, &mut dcb) == 0 {
            return Err("not a serial port".into());
        }
        dcb.BaudRate = baud;
        dcb.ByteSize = 8;
        dcb.Parity = 0;
        dcb.StopBits = 0;
        // binary mode, DTR on, RTS on, no flow control
        dcb._bitfield = 0x0001 | 0x0010 | 0x1000;
        if SetCommState(h, &dcb) == 0 {
            return Err(format!("{baud} baud is not accepted"));
        }
        let to = COMMTIMEOUTS { ReadIntervalTimeout: 0, ReadTotalTimeoutMultiplier: 0, ReadTotalTimeoutConstant: 1, WriteTotalTimeoutMultiplier: 0, WriteTotalTimeoutConstant: 1000 };
        SetCommTimeouts(h, &to);
    }
    Ok(f)
}

/// The serial ports of this machine ("COM3" ...), from the registry: nothing is opened, so no board restarts.
pub fn ports() -> Vec<String> {
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY_LOCAL_MACHINE, KEY_READ};
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let mut out = Vec::new();
    // SAFETY: plain registry calls with buffers of the sizes they are told
    unsafe {
        let mut key: *mut core::ffi::c_void = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide("HARDWARE\\DEVICEMAP\\SERIALCOMM").as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return out;
        }
        for i in 0..256 {
            let (mut name, mut data) = ([0u16; 256], [0u16; 64]);
            let (mut nlen, mut dlen) = (name.len() as u32, (data.len() * 2) as u32);
            if RegEnumValueW(key, i, name.as_mut_ptr(), &mut nlen, std::ptr::null(), std::ptr::null_mut(), data.as_mut_ptr() as *mut u8, &mut dlen) != 0 {
                break;
            }
            let n = (dlen as usize / 2).min(data.len());
            let s = String::from_utf16_lossy(&data[..n]).trim_end_matches('\0').to_string();
            if s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric()) && !s.is_empty() {
                out.push(s);
            }
        }
        RegCloseKey(key);
    }
    out.sort_by_key(|s| s.trim_start_matches("COM").parse::<u32>().unwrap_or(u32::MAX));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_expanded_and_the_set_up_survives_its_file() {
        assert_eq!(expand("G{angle}\\n", &[("angle", 45.0)]), b"G45\n");
        assert_eq!(expand("T {angle} \\x41\\\\", &[("angle", 22.5)]), b"T 22.50 A\\");
        // an escape that is none stays as it is
        assert_eq!(expand("a\\qb\\", &[]), b"a\\qb\\");
        let c = Cfg { port: "COM7".into(), baud: 9600, eol: 2, boot_ms: 500, cmds: vec![("Turn".into(), "G{angle}".into()), ("Relay 1".into(), "R1=1".into())] };
        assert!(from_text(&to_text(&c)) == c);
        // a damaged file gives the defaults for what is damaged
        let d = from_text("port=..\\evil\nbaud=x\neol=99\ncmd=no tab\n");
        assert!(d.port.is_empty() && d.baud == 115200 && d.eol == EOL.len() - 1 && d.cmds.is_empty());
    }
}
