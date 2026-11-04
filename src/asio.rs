//! ASIO, without the Steinberg SDK and without any crate: an ASIO driver is a COM object that is registered under
//! `HKLM\SOFTWARE\ASIO`; its methods are called through the vtable (the layout of the `IASIO` interface) and it calls back into a
//! small table of functions of ours. One driver at a time is open (drivers are one per process); everything that wants to read from
//! or play to it shares that one session, the driver thread of which owns the COM object.
//!
//! Device ids look like `asio:<driver name>`. Inputs get all the input channels of the driver interleaved as `f32`, outputs
//! deliver all output channels interleaved (the callback fills the ones it plays to, zero in the rest).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub const PREFIX: &str = "asio:";

pub fn is_asio(id: &str) -> bool {
    id.starts_with(PREFIX)
}

pub fn name_of(id: &str) -> &str {
    id.strip_prefix(PREFIX).unwrap_or(id)
}

/// What a driver offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub sr: u32,
    pub n_in: usize,
    pub n_out: usize,
    /// frames per buffer
    pub buf: usize,
}

pub type InCb = Box<dyn FnMut(&[f32]) + Send>;
pub type OutCb = Box<dyn FnMut(&mut [f32]) + Send>;

/// A running stream: of cpal, or a place in the ASIO session.
pub enum Held {
    Cpal(cpal::Stream),
    Asio(Port),
}

impl Held {
    /// A stream of an invalidated device can hang when dropped, so cpal's is leaked; an ASIO port is only a registration and is let go.
    pub fn abandon(self) {
        match self {
            Held::Cpal(s) => std::mem::forget(s),
            Held::Asio(p) => drop(p),
        }
    }
}

pub use imp::{drivers, forget_failed, open_input, open_output, probe, usable, Port};

#[cfg(all(windows, target_pointer_width = "64"))]
mod imp {
    use super::*;
    use parking_lot::{Mutex, RwLock};
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::Com::{CLSIDFromString, CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_READ};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetDesktopWindow, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, QS_ALLINPUT};
    use windows_sys::core::GUID;

    // ------------------------------------------------------------------ the interface of the driver

    #[repr(C)]
    struct ChannelInfo {
        channel: i32,
        is_input: i32,
        is_active: i32,
        group: i32,
        sample_type: i32,
        name: [u8; 32],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct BufferInfo {
        is_input: i32,
        channel: i32,
        buffers: [*mut c_void; 2],
    }

    #[repr(C)]
    struct Callbacks {
        buffer_switch: extern "C" fn(i32, i32),
        sample_rate_changed: extern "C" fn(f64),
        message: extern "C" fn(i32, i32, *mut c_void, *mut f64) -> i32,
        buffer_switch_time_info: extern "C" fn(*mut c_void, i32, i32) -> *mut c_void,
    }

    type Hr = i32;

    /// The `IASIO` vtable: IUnknown first, then the methods in the order of the header.
    #[repr(C)]
    struct Vtbl {
        _query_interface: usize,
        _add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        init: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
        _get_driver_name: usize,
        _get_driver_version: usize,
        get_error_message: unsafe extern "system" fn(*mut c_void, *mut u8),
        start: unsafe extern "system" fn(*mut c_void) -> i32,
        stop: unsafe extern "system" fn(*mut c_void) -> i32,
        get_channels: unsafe extern "system" fn(*mut c_void, *mut i32, *mut i32) -> i32,
        _get_latencies: usize,
        get_buffer_size: unsafe extern "system" fn(*mut c_void, *mut i32, *mut i32, *mut i32, *mut i32) -> i32,
        _can_sample_rate: usize,
        get_sample_rate: unsafe extern "system" fn(*mut c_void, *mut f64) -> i32,
        set_sample_rate: unsafe extern "system" fn(*mut c_void, f64) -> i32,
        _get_clock_sources: usize,
        _set_clock_source: usize,
        _get_sample_position: usize,
        get_channel_info: unsafe extern "system" fn(*mut c_void, *mut ChannelInfo) -> i32,
        create_buffers: unsafe extern "system" fn(*mut c_void, *mut BufferInfo, i32, i32, *const Callbacks) -> i32,
        dispose_buffers: unsafe extern "system" fn(*mut c_void) -> i32,
        _control_panel: usize,
        _future: usize,
        output_ready: unsafe extern "system" fn(*mut c_void) -> i32,
    }

    struct Driver(*mut c_void);

    impl Driver {
        fn v(&self) -> &Vtbl {
            // SAFETY: the pointer is a live COM object; its first word is the pointer to the vtable
            unsafe { &**(self.0 as *const *const Vtbl) }
        }
    }

    // ------------------------------------------------------------------ registry

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The drivers registered on this machine: (name, CLSID).
    fn registered() -> Vec<(String, String)> {
        let mut out = Vec::new();
        // SAFETY: plain registry calls with buffers of the sizes they are told
        unsafe {
            let mut root: *mut c_void = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide("SOFTWARE\\ASIO").as_ptr(), 0, KEY_READ, &mut root) != 0 {
                return out;
            }
            let mut i = 0;
            loop {
                let mut name = [0u16; 256];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(root, i, name.as_mut_ptr(), &mut len, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
                    break;
                }
                i += 1;
                let name = String::from_utf16_lossy(&name[..len as usize]);
                let mut key: *mut c_void = std::ptr::null_mut();
                if RegOpenKeyExW(root, wide(&name).as_ptr(), 0, KEY_READ, &mut key) == 0 {
                    let mut buf = [0u16; 128];
                    let mut size = (buf.len() * 2) as u32;
                    let mut typ = 0;
                    if RegQueryValueExW(key, wide("CLSID").as_ptr(), std::ptr::null(), &mut typ, buf.as_mut_ptr() as *mut u8, &mut size) == 0 {
                        let n = (size as usize / 2).min(buf.len());
                        let clsid = String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string();
                        out.push((name, clsid));
                    }
                    RegCloseKey(key);
                }
            }
            RegCloseKey(root);
        }
        out.sort();
        out
    }

    /// Names of the installed ASIO drivers.
    pub fn drivers() -> Vec<String> {
        registered().into_iter().map(|d| d.0).collect()
    }

    // ------------------------------------------------------------------ sample formats

    /// Bits of the sample types that are handled (Windows is little endian): 16, 24 and 32 bit integers, float, double, and the
    /// 32 bit containers with 16 / 18 / 20 / 24 valid bits.
    fn supported(t: i32) -> bool {
        matches!(t, 16..=20 | 24..=27)
    }

    unsafe fn read(p: *const u8, t: i32, i: usize) -> f32 {
        // SAFETY: the caller keeps `i` inside the buffer of the driver
        unsafe {
            match t {
                16 => (p as *const i16).add(i).read_unaligned() as f32 / 32768.0,
                17 => {
                    let q = p.add(i * 3);
                    let v = ((q.add(2).read() as i8 as i32) << 16) | ((q.add(1).read() as i32) << 8) | q.read() as i32;
                    v as f32 / 8_388_608.0
                }
                18 => (p as *const i32).add(i).read_unaligned() as f32 / 2_147_483_648.0,
                19 => (p as *const f32).add(i).read_unaligned(),
                20 => (p as *const f64).add(i).read_unaligned() as f32,
                24 => (p as *const i32).add(i).read_unaligned() as f32 / 32768.0,
                25 => (p as *const i32).add(i).read_unaligned() as f32 / 131_072.0,
                26 => (p as *const i32).add(i).read_unaligned() as f32 / 524_288.0,
                27 => (p as *const i32).add(i).read_unaligned() as f32 / 8_388_608.0,
                _ => 0.0,
            }
        }
    }

    unsafe fn write(p: *mut u8, t: i32, i: usize, v: f32) {
        let v = if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
        // SAFETY: as for `read`
        unsafe {
            match t {
                16 => (p as *mut i16).add(i).write_unaligned((v * 32767.0) as i16),
                17 => {
                    let x = (v * 8_388_607.0) as i32;
                    let q = p.add(i * 3);
                    q.write(x as u8);
                    q.add(1).write((x >> 8) as u8);
                    q.add(2).write((x >> 16) as u8);
                }
                18 => (p as *mut i32).add(i).write_unaligned((v as f64 * 2_147_483_647.0) as i32),
                19 => (p as *mut f32).add(i).write_unaligned(v),
                20 => (p as *mut f64).add(i).write_unaligned(v as f64),
                24 => (p as *mut i32).add(i).write_unaligned((v * 32767.0) as i32),
                25 => (p as *mut i32).add(i).write_unaligned((v * 131_071.0) as i32),
                26 => (p as *mut i32).add(i).write_unaligned((v * 524_287.0) as i32),
                27 => (p as *mut i32).add(i).write_unaligned((v * 8_388_607.0) as i32),
                _ => {}
            }
        }
    }

    #[cfg(test)]
    pub unsafe fn read_for_test(p: *const u8, t: i32, i: usize) -> f32 {
        unsafe { read(p, t, i) }
    }

    #[cfg(test)]
    pub unsafe fn write_for_test(p: *mut u8, t: i32, i: usize, v: f32) {
        unsafe { write(p, t, i, v) }
    }

    // ------------------------------------------------------------------ the session

    struct Sub<C> {
        id: u64,
        cb: C,
        /// set when the driver asks for a reset: whoever holds the port rebuilds
        dead: Arc<AtomicBool>,
    }

    #[derive(Default)]
    struct Subs {
        inputs: Vec<Sub<InCb>>,
        outputs: Vec<Sub<OutCb>>,
        next: u64,
        /// scratch of the callback: interleaved inputs, the mix of the outputs, one output
        inter_in: Vec<f32>,
        inter_out: Vec<f32>,
        tmp_out: Vec<f32>,
    }

    /// What the callback of the driver needs.
    struct Live {
        subs: Arc<Mutex<Subs>>,
        info: Info,
        bufs: Vec<BufferInfo>,
        types: Vec<i32>,
        stop: Arc<AtomicBool>,
    }

    // SAFETY: the pointers are the buffers of the driver, valid from createBuffers to disposeBuffers, which the session thread
    // does after it has taken `Live` out of `LIVE`
    unsafe impl Send for Live {}
    unsafe impl Sync for Live {}

    static LIVE: RwLock<Option<Arc<Live>>> = RwLock::new(None);

    impl Live {
        fn process(&self, idx: usize) {
            let Some(mut s) = self.subs.try_lock() else { return };
            let s = &mut *s;
            let (n_in, n_out, frames) = (self.info.n_in, self.info.n_out, self.info.buf);
            let idx = idx & 1;
            if !s.inputs.is_empty() && n_in > 0 {
                s.inter_in.resize(n_in * frames, 0.0);
                for c in 0..n_in {
                    let p = self.bufs[c].buffers[idx] as *const u8;
                    if p.is_null() {
                        continue;
                    }
                    for f in 0..frames {
                        // SAFETY: `frames` samples of this channel were allocated by the driver
                        s.inter_in[f * n_in + c] = unsafe { read(p, self.types[c], f) };
                    }
                }
                let data = &s.inter_in[..n_in * frames];
                for sub in s.inputs.iter_mut() {
                    (sub.cb)(data);
                }
            }
            if n_out > 0 {
                s.inter_out.clear();
                s.inter_out.resize(n_out * frames, 0.0);
                for (k, sub) in s.outputs.iter_mut().enumerate() {
                    if k == 0 {
                        (sub.cb)(&mut s.inter_out);
                    } else {
                        s.tmp_out.clear();
                        s.tmp_out.resize(n_out * frames, 0.0);
                        (sub.cb)(&mut s.tmp_out);
                        for (a, b) in s.inter_out.iter_mut().zip(s.tmp_out.iter()) {
                            *a += *b;
                        }
                    }
                }
                for c in 0..n_out {
                    let p = self.bufs[n_in + c].buffers[idx] as *mut u8;
                    if p.is_null() {
                        continue;
                    }
                    for f in 0..frames {
                        // SAFETY: as above, for the output channel
                        unsafe { write(p, self.types[n_in + c], f, s.inter_out[f * n_out + c]) };
                    }
                }
            }
        }

        /// The driver wants the session rebuilt (reset request, rate or buffer size changed).
        fn reset(&self) {
            {
                let s = self.subs.lock();
                for d in s.inputs.iter().map(|x| &x.dead).chain(s.outputs.iter().map(|x| &x.dead)) {
                    d.store(true, Relaxed);
                }
            }
            self.stop.store(true, Relaxed);
        }
    }

    extern "C" fn cb_switch(index: i32, _direct: i32) {
        let live = LIVE.read().clone();
        if let Some(l) = live {
            l.process(index.max(0) as usize);
        }
    }

    extern "C" fn cb_rate(_sr: f64) {
        if let Some(l) = LIVE.read().clone() {
            crate::log::log("asio: the sample rate of the driver changed");
            l.reset();
        }
    }

    extern "C" fn cb_message(selector: i32, value: i32, _msg: *mut c_void, _opt: *mut f64) -> i32 {
        match selector {
            // kAsioSelectorSupported: which messages we understand
            1 => matches!(value, 2 | 3 | 4 | 5 | 6) as i32,
            // kAsioEngineVersion
            2 => 2,
            // kAsioResetRequest, kAsioBufferSizeChange: the session is rebuilt
            3 | 4 => {
                if let Some(l) = LIVE.read().clone() {
                    crate::log::log("asio: the driver asked for a reset");
                    l.reset();
                }
                1
            }
            // kAsioResyncRequest, kAsioLatenciesChanged
            5 | 6 => 1,
            _ => 0,
        }
    }

    extern "C" fn cb_time(_t: *mut c_void, index: i32, direct: i32) -> *mut c_void {
        cb_switch(index, direct);
        std::ptr::null_mut()
    }

    struct Session {
        name: String,
        stop: Arc<AtomicBool>,
        done: Arc<AtomicBool>,
        subs: Arc<Mutex<Subs>>,
        info: Info,
    }

    static SESSION: Mutex<Option<Session>> = Mutex::new(None);
    /// What was found out about the drivers (None: it did not open).
    static CACHE: Mutex<Option<HashMap<String, Option<Info>>>> = Mutex::new(None);

    fn driver_error(d: &Driver) -> String {
        let mut buf = [0u8; 128];
        // SAFETY: the driver writes a NUL terminated string of at most 124 characters
        unsafe { (d.v().get_error_message)(d.0, buf.as_mut_ptr()) };
        let n = buf.iter().position(|b| *b == 0).unwrap_or(0);
        String::from_utf8_lossy(&buf[..n]).to_string()
    }

    /// The thread that owns the driver: loads it, starts it, pumps its messages until it is told to stop, then shuts it down.
    fn session_thread(name: String, clsid: String, subs: Arc<Mutex<Subs>>, stop: Arc<AtomicBool>, done: Arc<AtomicBool>, ready: std::sync::mpsc::Sender<Result<Info, String>>) {
        // SAFETY: COM and the driver are used from this thread only (the callbacks come from the driver's own thread)
        unsafe {
            CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
            let r = (|| -> Result<(Driver, Info, Vec<BufferInfo>, Vec<i32>), String> {
                let mut guid = std::mem::zeroed::<GUID>();
                if CLSIDFromString(wide(&clsid).as_ptr(), &mut guid) != 0 {
                    return Err(format!("bad driver id {clsid}"));
                }
                let mut ptr: *mut c_void = std::ptr::null_mut();
                // (the id of the class doubles as the id of the interface: that is how ASIO drivers are made)
                let hr = CoCreateInstance(&guid, std::ptr::null_mut(), CLSCTX_INPROC_SERVER, &guid, &mut ptr);
                if hr != 0 || ptr.is_null() {
                    return Err(format!("{name}: the driver could not be loaded (error {hr:#x})"));
                }
                let d = Driver(ptr);
                let fail = |d: &Driver, what: &str| -> String {
                    let msg = driver_error(d);
                    (d.v().release)(d.0);
                    if msg.is_empty() { what.to_string() } else { format!("{what}: {msg}") }
                };
                if (d.v().init)(d.0, GetDesktopWindow() as *mut c_void) == 0 {
                    return Err(fail(&d, &format!("{name}: the driver did not start (is the device connected?)")));
                }
                let (mut n_in, mut n_out) = (0i32, 0i32);
                if (d.v().get_channels)(d.0, &mut n_in, &mut n_out) != 0 || n_in + n_out <= 0 {
                    return Err(fail(&d, &format!("{name}: no channels")));
                }
                let (mut min, mut max, mut pref, mut gran) = (0i32, 0i32, 0i32, 0i32);
                if (d.v().get_buffer_size)(d.0, &mut min, &mut max, &mut pref, &mut gran) != 0 || pref <= 0 {
                    return Err(fail(&d, &format!("{name}: no buffer size")));
                }
                let mut sr = 0f64;
                (d.v().get_sample_rate)(d.0, &mut sr);
                if !(8000.0..=768_000.0).contains(&sr) {
                    sr = 48000.0;
                    (d.v().set_sample_rate)(d.0, sr);
                    (d.v().get_sample_rate)(d.0, &mut sr);
                }
                // all the channels, so that whoever opens the driver later finds its channels in
                let (n_in, n_out) = ((n_in.max(0) as usize).min(64), (n_out.max(0) as usize).min(64));
                let mut bufs: Vec<BufferInfo> = (0..n_in).map(|c| BufferInfo { is_input: 1, channel: c as i32, buffers: [std::ptr::null_mut(); 2] }).chain((0..n_out).map(|c| BufferInfo { is_input: 0, channel: c as i32, buffers: [std::ptr::null_mut(); 2] })).collect();
                let mut types = Vec::new();
                for b in &bufs {
                    let mut ci = ChannelInfo { channel: b.channel, is_input: b.is_input, is_active: 0, group: 0, sample_type: 0, name: [0; 32] };
                    if (d.v().get_channel_info)(d.0, &mut ci) != 0 {
                        return Err(fail(&d, &format!("{name}: channel info")));
                    }
                    if !supported(ci.sample_type) {
                        return Err(fail(&d, &format!("{name}: sample format {} is not supported", ci.sample_type)));
                    }
                    types.push(ci.sample_type);
                }
                let info = Info { sr: sr.round() as u32, n_in, n_out, buf: pref as usize };
                static CBS: Callbacks = Callbacks { buffer_switch: cb_switch, sample_rate_changed: cb_rate, message: cb_message, buffer_switch_time_info: cb_time };
                // the callbacks may fire as soon as the buffers exist: have `Live` ready, its pointers are filled in below
                let e = (d.v().create_buffers)(d.0, bufs.as_mut_ptr(), bufs.len() as i32, pref, &CBS);
                if e != 0 {
                    return Err(fail(&d, &format!("{name}: the buffers could not be made (error {e})")));
                }
                Ok((d, info, bufs, types))
            })();
            let (d, info, bufs, types) = match r {
                Ok(v) => v,
                Err(e) => {
                    let _ = ready.send(Err(e));
                    CoUninitialize();
                    done.store(true, Relaxed);
                    return;
                }
            };
            *LIVE.write() = Some(Arc::new(Live { subs: subs.clone(), info: info.clone(), bufs, types, stop: stop.clone() }));
            if (d.v().start)(d.0) != 0 {
                let msg = driver_error(&d);
                *LIVE.write() = None;
                (d.v().dispose_buffers)(d.0);
                (d.v().release)(d.0);
                let _ = ready.send(Err(format!("{name}: the driver could not be started {msg}")));
                CoUninitialize();
                done.store(true, Relaxed);
                return;
            }
            crate::log::log(&format!("asio: {name} running: {} Hz, {} in, {} out, {} frames", info.sr, info.n_in, info.n_out, info.buf));
            let _ = ready.send(Ok(info));
            let _ = (d.v().output_ready)(d.0);
            // keep the driver alive, pumping messages for it (many drivers make a window on this thread)
            while !stop.load(Relaxed) {
                MsgWaitForMultipleObjects(0, std::ptr::null(), 0, 20, QS_ALLINPUT);
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            (d.v().stop)(d.0);
            *LIVE.write() = None;
            // the callback may still be inside `process`: wait for the lock it holds
            drop(subs.lock());
            (d.v().dispose_buffers)(d.0);
            (d.v().release)(d.0);
            CoUninitialize();
            crate::log::log(&format!("asio: {name} closed"));
            done.store(true, Relaxed);
        }
    }

    /// A place in the running session; dropping it lets go (the last one closes the driver).
    pub struct Port {
        id: u64,
        name: String,
    }

    impl Drop for Port {
        fn drop(&mut self) {
            let g = SESSION.lock();
            if let Some(s) = g.as_ref().filter(|s| s.name == self.name) {
                let mut subs = s.subs.lock();
                subs.inputs.retain(|x| x.id != self.id);
                subs.outputs.retain(|x| x.id != self.id);
                if subs.inputs.is_empty() && subs.outputs.is_empty() {
                    s.stop.store(true, Relaxed);
                }
            }
        }
    }

    /// Make sure the session of this driver runs; `add` puts the callbacks in (None: nothing was added).
    fn open(name: &str, add: impl FnOnce(&Info, &mut Subs) -> Option<u64>) -> Result<(Option<Port>, Info), String> {
        let clsid = registered().into_iter().find(|d| d.0 == name).map(|d| d.1).ok_or_else(|| format!("ASIO driver {name} not found"))?;
        let mut add = Some(add);
        let started = Instant::now();
        loop {
            let mut g = SESSION.lock();
            // what is there: nothing, a session to use, one to wait for, or one that blocks
            enum St {
                None,
                Finished,
                Use,
                Wait,
                Busy(String),
            }
            let st = match g.as_ref() {
                None => St::None,
                Some(s) if s.done.load(Relaxed) => St::Finished,
                Some(s) if s.name == name && !s.stop.load(Relaxed) => St::Use,
                Some(s) => {
                    let busy = s.name != name && {
                        let subs = s.subs.lock();
                        !subs.inputs.is_empty() || !subs.outputs.is_empty()
                    };
                    if busy { St::Busy(s.name.clone()) } else { St::Wait }
                }
            };
            match st {
                St::Busy(other) => return Err(format!("ASIO driver {other} is in use: only one ASIO driver can run at a time")),
                St::Use => {
                    let s = g.as_ref().unwrap();
                    let id = add.take().and_then(|f| f(&s.info, &mut s.subs.lock()));
                    return Ok((id.map(|id| Port { id, name: name.to_string() }), s.info.clone()));
                }
                St::Wait => {
                    // another driver without users, or this one on its way out: let it go, then try again
                    if let Some(s) = g.as_ref() {
                        s.stop.store(true, Relaxed);
                    }
                    drop(g);
                    if started.elapsed() > Duration::from_secs(6) {
                        return Err(format!("{name}: the previous ASIO driver did not close"));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                St::Finished => *g = None,
                St::None => {}
            }
            // start a new one
            let (subs, stop, done) = (Arc::new(Mutex::new(Subs::default())), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
            let (tx, rx) = std::sync::mpsc::channel();
            let (n2, s2, st2, d2) = (name.to_string(), subs.clone(), stop.clone(), done.clone());
            if std::thread::Builder::new().name("asio".into()).spawn(move || session_thread(n2, clsid, s2, st2, d2, tx)).is_err() {
                return Err("the ASIO thread could not be started".into());
            }
            // (the lock is held while the driver loads: nobody else opens one meanwhile)
            let info = match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(i)) => i,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    stop.store(true, Relaxed);
                    return Err(format!("{name}: the driver did not answer"));
                }
            };
            let id = add.take().and_then(|f| f(&info, &mut subs.lock()));
            if id.is_none() {
                // only asked about: nothing uses it
                stop.store(true, Relaxed);
            }
            CACHE.lock().get_or_insert_with(HashMap::new).insert(name.to_string(), Some(info.clone()));
            *g = Some(Session { name: name.to_string(), stop, done, subs, info: info.clone() });
            return Ok((id.map(|id| Port { id, name: name.to_string() }), info));
        }
    }

    /// Channels and rate of a driver (it is opened and closed again for that; the answer is kept).
    pub fn probe(name: &str) -> Option<Info> {
        if let Some(c) = CACHE.lock().as_ref().and_then(|m| m.get(name)).cloned() {
            return c;
        }
        let r = open(name, |_, _| None);
        let info = match r {
            Ok((_, i)) => Some(i),
            Err(e) => {
                crate::log::log(&format!("asio: {e}"));
                // not opened because another driver is in use: say nothing about it
                if e.contains("in use") {
                    return None;
                }
                None
            }
        };
        CACHE.lock().get_or_insert_with(HashMap::new).insert(name.to_string(), info.clone());
        info
    }

    /// Drivers that did not open are tried again at the next probe (a device may have been plugged in since).
    pub fn forget_failed() {
        if let Some(m) = CACHE.lock().as_mut() {
            m.retain(|_, v| v.is_some());
        }
    }

    /// The drivers that can be offered: the ones that opened when they were probed, or have not been probed yet.
    pub fn usable() -> Vec<(String, Option<Info>)> {
        let cache = CACHE.lock();
        drivers()
            .into_iter()
            .filter_map(|n| match cache.as_ref().and_then(|m| m.get(&n)) {
                Some(None) => None,
                Some(Some(i)) => Some((n, Some(i.clone()))),
                None => Some((n, None)),
            })
            .collect()
    }

    /// Read from the inputs of the driver. `make` builds the callback once the driver is running and its format is known.
    pub fn open_input(name: &str, dead: Arc<AtomicBool>, make: impl FnOnce(&Info) -> InCb) -> Result<(Port, Info), String> {
        let (p, info) = open(name, |info, subs| {
            if info.n_in == 0 {
                return None;
            }
            let id = subs.next;
            subs.next += 1;
            subs.inputs.push(Sub { id, cb: make(info), dead: dead.clone() });
            Some(id)
        })?;
        p.map(|p| (p, info)).ok_or_else(|| format!("{name} has no inputs"))
    }

    /// Play to the outputs of the driver.
    pub fn open_output(name: &str, dead: Arc<AtomicBool>, make: impl FnOnce(&Info) -> OutCb) -> Result<(Port, Info), String> {
        let (p, info) = open(name, |info, subs| {
            if info.n_out == 0 {
                return None;
            }
            let id = subs.next;
            subs.next += 1;
            subs.outputs.push(Sub { id, cb: make(info), dead: dead.clone() });
            Some(id)
        })?;
        p.map(|p| (p, info)).ok_or_else(|| format!("{name} has no outputs"))
    }
}

#[cfg(not(all(windows, target_pointer_width = "64")))]
mod imp {
    use super::*;

    pub struct Port;

    pub fn drivers() -> Vec<String> {
        Vec::new()
    }

    pub fn probe(_name: &str) -> Option<Info> {
        None
    }

    pub fn forget_failed() {}

    pub fn usable() -> Vec<(String, Option<Info>)> {
        Vec::new()
    }

    pub fn open_input(_name: &str, _dead: Arc<AtomicBool>, _make: impl FnOnce(&Info) -> InCb) -> Result<(Port, Info), String> {
        Err("ASIO is only available on 64 bit Windows".into())
    }

    pub fn open_output(_name: &str, _dead: Arc<AtomicBool>, _make: impl FnOnce(&Info) -> OutCb) -> Result<(Port, Info), String> {
        Err("ASIO is only available on 64 bit Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_name_the_driver() {
        assert!(is_asio("asio:Focusrite USB ASIO"));
        assert!(!is_asio("wasapi:abc"));
        assert_eq!(name_of("asio:Focusrite USB ASIO"), "Focusrite USB ASIO");
    }

    /// Opens every installed driver in turn and prints what it offers (needs the hardware: cargo test --release probe_installed -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn probe_installed_drivers() {
        for n in drivers() {
            let t = std::time::Instant::now();
            let i = probe(&n);
            println!("{n}: {i:?} ({} ms)", t.elapsed().as_millis());
        }
    }

    /// Reads one second from the first driver with inputs and plays one second of silence to the first with outputs (needs the hardware).
    #[test]
    #[ignore]
    fn run_a_second_of_audio() {
        use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
        let dead = Arc::new(AtomicBool::new(false));
        let all: Vec<(String, Info)> = drivers().into_iter().filter_map(|n| probe(&n).map(|i| (n, i))).collect();
        let inp = all.iter().find(|d| d.1.n_in > 0).expect("a driver with inputs");
        let frames = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU64::new(0));
        let (f2, p2) = (frames.clone(), peak.clone());
        let (port, info) = open_input(&inp.0, dead.clone(), move |i| {
            let n_in = i.n_in;
            Box::new(move |d: &[f32]| {
                f2.fetch_add((d.len() / n_in) as u64, Relaxed);
                let m = d.iter().fold(0f32, |m, v| m.max(v.abs()));
                p2.fetch_max(m.to_bits() as u64, Relaxed);
            })
        })
        .expect("input opens");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let got = frames.load(Relaxed);
        println!("{}: {} frames in 1.1 s at {} Hz, peak {}", inp.0, got, info.sr, f32::from_bits(peak.load(Relaxed) as u32));
        assert!(got as f64 > 0.7 * info.sr as f64 && (got as f64) < 1.5 * info.sr as f64, "{got}");
        drop(port);
        // another driver while the first closes
        let out = all.iter().find(|d| d.1.n_out > 0 && d.0 != inp.0).or(all.iter().find(|d| d.1.n_out > 0)).expect("a driver with outputs");
        let blocks = Arc::new(AtomicU64::new(0));
        let b2 = blocks.clone();
        let (port, oi) = open_output(&out.0, dead.clone(), move |_| Box::new(move |d: &mut [f32]| {
            d.iter_mut().for_each(|v| *v = 0.0);
            b2.fetch_add(d.len() as u64, Relaxed);
        }))
        .expect("output opens");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let got = blocks.load(Relaxed) / oi.n_out as u64;
        println!("{}: {} frames played in 1.1 s at {} Hz", out.0, got, oi.sr);
        assert!(got as f64 > 0.7 * oi.sr as f64, "{got}");
        drop(port);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    /// The sample formats round trip (every supported one).
    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn sample_formats_round_trip() {
        for t in [16, 17, 18, 19, 20, 24, 25, 26, 27] {
            let mut buf = vec![0u8; 8 * 8];
            for (i, v) in [0.0f32, 0.5, -0.5, 0.999, -0.999].iter().enumerate() {
                unsafe { imp::write_for_test(buf.as_mut_ptr(), t, i, *v) };
            }
            for (i, v) in [0.0f32, 0.5, -0.5, 0.999, -0.999].iter().enumerate() {
                let back = unsafe { imp::read_for_test(buf.as_ptr(), t, i) };
                assert!((back - v).abs() < 1e-3, "type {t}: {v} came back as {back}");
            }
        }
    }
}

/// The whole application path with real drivers (needs the hardware: cargo test --release through_the_app -- --ignored --nocapture).
#[cfg(all(test, windows))]
mod hardware_tests {
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    use std::sync::Arc;
    use std::time::Duration;

    fn pick(inputs: bool) -> Option<String> {
        super::drivers().into_iter().find(|n| super::probe(n).is_some_and(|i| if inputs { i.n_in > 0 } else { i.n_out > 0 }))
    }

    #[test]
    #[ignore]
    fn through_the_app() {
        let name = pick(true).expect("a driver with inputs");
        let id = format!("{}{name}", super::PREFIX);
        let info = super::probe(&name).unwrap();
        println!("using {name}: {info:?}");
        // the lists offer it
        assert!(crate::audio::list_devices().iter().any(|d| d.id == id), "in the list of inputs");
        assert!(crate::tonegen::list_output_devices().iter().any(|d| d.id == id) || info.n_out == 0, "in the list of outputs");
        let capdev = crate::tools::daw::capture::probe_all();
        let e = capdev.iter().find(|d| d.id == id).expect("in the capture list");
        assert_eq!((e.ch, e.sr), (info.n_in, info.sr));
        println!("label: {}", crate::tools::daw::capture::short_label(&e.label));
        // analysis input, timeline capture, generator and monitor on the same driver at once
        let sh = Arc::new(crate::shared::Shared::new());
        let _audio = crate::audio::start(sh.clone(), id.clone());
        let cap = crate::tools::daw::capture::Capture::start(id.clone());
        let status = Arc::new(Mutex::new(String::new()));
        let gen_p = Arc::new(Mutex::new(crate::tonegen::GenParams::default()));
        let _gen = crate::tonegen::start(gen_p, id.clone(), status.clone());
        let q = Arc::new(Mutex::new(crate::tools::monitor::Queue::default()));
        let mstatus = Arc::new(Mutex::new(String::new()));
        let _mon = crate::tools::monitor::start(q.clone(), Arc::new(AtomicU32::new(0f32.to_bits())), Some(id.clone()), mstatus.clone());
        std::thread::sleep(Duration::from_millis(2500));
        let ring = sh.ring.lock().total;
        let cap_frames = cap.total_frames();
        println!("analysis ring {ring} frames, capture {cap_frames} frames, status '{}', generator '{}', monitor '{}'", sh.status.lock(), status.lock(), mstatus.lock());
        assert!(ring as f64 > 0.8 * info.sr as f64, "analysis input got {ring}");
        assert!(cap_frames as f64 > 0.8 * info.sr as f64, "capture got {cap_frames}");
        assert!(cap.format().is_some_and(|f| f.0 == info.sr && f.1 == info.n_in));
        if info.n_out > 0 {
            assert!(status.lock().starts_with("ASIO"), "generator: {}", status.lock());
            assert!(mstatus.lock().starts_with("ASIO"), "monitor: {}", mstatus.lock());
        }
    }
}
