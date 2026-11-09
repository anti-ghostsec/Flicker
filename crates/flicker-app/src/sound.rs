//! Sound for the preview. ffmpeg mixes the timeline into 48 kHz stereo PCM
//! and a thread writes it to the sound card; how much has really been played
//! is the clock the picture follows.
//!
//! On Linux the sound goes to ALSA's "default" device through
//! `libasound.so.2`, loaded when playback starts (so building Flicker needs
//! no ALSA headers, and a machine without ALSA still runs it, silently).
//! Elsewhere, and when no device opens, playback is silent and the picture
//! keeps time by the wall clock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use flicker_core::{Engine, TimelinePreview, SAMPLE_RATE};

const CHANNELS: u32 = 2;
/// Bytes per sample frame (two 16-bit channels).
const FRAME: usize = 4;
/// How much the sound card buffers.
const LATENCY: Duration = Duration::from_millis(120);
/// Written per round: about 21 ms.
const CHUNK_FRAMES: usize = 1024;

/// Where sound playback is.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Opening the device and starting ffmpeg.
    Starting,
    /// Played this much (since playback started) as of the given instant.
    Playing(Duration, Instant),
    /// Everything has been played.
    Finished,
    /// No sound: the reason, for the status bar.
    Unavailable(String),
}

/// Plays the timeline's sound from a position until dropped.
pub struct Playback {
    stop: Arc<AtomicBool>,
    /// Sound waits for this before it starts, so it starts with the picture.
    go: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
    thread: Option<JoinHandle<()>>,
}

impl Playback {
    pub fn start(engine: Arc<dyn Engine>, req: TimelinePreview) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let go = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(State::Starting));
        let (s, g, st) = (stop.clone(), go.clone(), state.clone());
        let thread = thread::Builder::new()
            .name("sound".into())
            .spawn(move || {
                let result = play(&*engine, &req, &s, &g, &st);
                if let Err(e) = result {
                    set(&st, State::Unavailable(e));
                }
            })
            .ok();
        Self { stop, go, state, thread }
    }

    /// The picture is ready: start the sound.
    pub fn go(&self) {
        self.go.store(true, Ordering::SeqCst);
    }

    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or(State::Unavailable("sound thread failed".into()))
    }

    /// How far playback has got, smoothed between the thread's updates.
    /// `None` while starting.
    pub fn clock(&self) -> Option<Duration> {
        match self.state() {
            State::Playing(played, at) => Some(played + at.elapsed().min(Duration::from_millis(60))),
            _ => None,
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The thread notices within one chunk (about 21 ms); don't wait on it.
        drop(self.thread.take());
    }
}

fn set(state: &Mutex<State>, s: State) {
    if let Ok(mut g) = state.lock() {
        *g = s;
    }
}

fn play(
    engine: &dyn Engine,
    req: &TimelinePreview,
    stop: &AtomicBool,
    go: &AtomicBool,
    state: &Mutex<State>,
) -> Result<(), String> {
    let mut out = device::Pcm::open(SAMPLE_RATE, CHANNELS, LATENCY)?;
    let mut source = engine.open_timeline_sound(req).map_err(|e| format!("no sound: {e}"))?;
    let mut buf = vec![0u8; CHUNK_FRAMES * FRAME];
    // Have the first sound ready, then wait for the first picture (but not
    // forever: a picture that never comes mustn't keep the sound back).
    let mut first = source.read(&mut buf).map_err(|e| e.to_string())?;
    let waiting = Instant::now();
    while !go.load(Ordering::SeqCst) && !stop.load(Ordering::SeqCst) && waiting.elapsed() < Duration::from_secs(3) {
        thread::sleep(Duration::from_millis(2));
    }
    let mut samples: Vec<i16> = Vec::with_capacity(CHUNK_FRAMES * CHANNELS as usize);
    let mut written: u64 = 0;
    let mut first_write: Option<Instant> = None;
    let rate = SAMPLE_RATE as f64;
    loop {
        if stop.load(Ordering::SeqCst) {
            out.stop();
            return Ok(());
        }
        let n = match std::mem::take(&mut first) {
            0 => source.read(&mut buf).map_err(|e| e.to_string())?,
            n => n,
        };
        if n == 0 {
            break;
        }
        samples.clear();
        samples.extend(buf[..n].as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)));
        out.write(&samples)?;
        written += (n / FRAME) as u64;
        let started = *first_write.get_or_insert_with(Instant::now);
        // Never more than has had time to play (devices that don't report
        // their delay, like the null device, would otherwise run ahead).
        let played = (written.saturating_sub(out.delay()) as f64 / rate).min(started.elapsed().as_secs_f64());
        set(state, State::Playing(Duration::from_secs_f64(played), Instant::now()));
        // A device that takes sound faster than it plays (ALSA's null
        // device does) mustn't run ahead of the picture.
        let ahead = written as f64 / rate - started.elapsed().as_secs_f64() - LATENCY.as_secs_f64();
        if ahead > 0.0 {
            thread::sleep(Duration::from_secs_f64(ahead));
        }
    }
    // Let the buffered tail play out.
    let started = first_write.unwrap_or_else(Instant::now);
    let total = Duration::from_secs_f64(written as f64 / rate);
    while !stop.load(Ordering::SeqCst) {
        let delay = out.delay();
        let played = (written.saturating_sub(delay) as f64 / rate).min(started.elapsed().as_secs_f64());
        set(state, State::Playing(Duration::from_secs_f64(played), Instant::now()));
        if delay == 0 && started.elapsed() >= total {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    set(state, State::Finished);
    Ok(())
}

#[cfg(target_os = "linux")]
mod device {
    //! A minimal binding to the few libasound calls playback needs.

    use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr};
    use std::sync::OnceLock;
    use std::time::Duration;

    extern "C" {
        fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    const RTLD_NOW: c_int = 2;
    const SND_PCM_STREAM_PLAYBACK: c_int = 0;
    const SND_PCM_FORMAT_S16_LE: c_int = 2;
    const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;

    type Open = unsafe extern "C" fn(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int;
    type SetParams = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int;
    type Writei = unsafe extern "C" fn(*mut c_void, *const c_void, c_ulong) -> c_long;
    type Recover = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
    type Delay = unsafe extern "C" fn(*mut c_void, *mut c_long) -> c_int;
    type Handle = unsafe extern "C" fn(*mut c_void) -> c_int;
    type StrError = unsafe extern "C" fn(c_int) -> *const c_char;

    struct Lib {
        open: Open,
        set_params: SetParams,
        writei: Writei,
        recover: Recover,
        delay: Delay,
        drop: Handle,
        close: Handle,
        strerror: StrError,
    }

    fn lib() -> Option<&'static Lib> {
        static LIB: OnceLock<Option<Lib>> = OnceLock::new();
        LIB.get_or_init(|| unsafe {
            let h = dlopen(c"libasound.so.2".as_ptr(), RTLD_NOW);
            if h.is_null() {
                return None;
            }
            let sym = |name: &CStr| {
                let p = dlsym(h, name.as_ptr());
                (!p.is_null()).then_some(p)
            };
            // SAFETY: the signatures match alsa-lib's <alsa/pcm.h>.
            Some(Lib {
                open: std::mem::transmute::<*mut c_void, Open>(sym(c"snd_pcm_open")?),
                set_params: std::mem::transmute::<*mut c_void, SetParams>(sym(c"snd_pcm_set_params")?),
                writei: std::mem::transmute::<*mut c_void, Writei>(sym(c"snd_pcm_writei")?),
                recover: std::mem::transmute::<*mut c_void, Recover>(sym(c"snd_pcm_recover")?),
                delay: std::mem::transmute::<*mut c_void, Delay>(sym(c"snd_pcm_delay")?),
                drop: std::mem::transmute::<*mut c_void, Handle>(sym(c"snd_pcm_drop")?),
                close: std::mem::transmute::<*mut c_void, Handle>(sym(c"snd_pcm_close")?),
                strerror: std::mem::transmute::<*mut c_void, StrError>(sym(c"snd_strerror")?),
            })
        })
        .as_ref()
    }

    /// An open ALSA playback device.
    pub struct Pcm {
        pcm: *mut c_void,
        lib: &'static Lib,
    }

    // The handle is only ever used from the sound thread that opened it.
    unsafe impl Send for Pcm {}

    impl Pcm {
        pub fn open(rate: u32, channels: u32, latency: Duration) -> Result<Self, String> {
            let lib = lib().ok_or("no sound: ALSA (libasound.so.2) isn't installed")?;
            let mut pcm = std::ptr::null_mut();
            unsafe {
                let err = (lib.open)(&mut pcm, c"default".as_ptr(), SND_PCM_STREAM_PLAYBACK, 0);
                if err < 0 {
                    return Err(format!("no sound device: {}", error(lib, err)));
                }
                let p = Self { pcm, lib };
                let us = latency.as_micros() as c_uint;
                let err =
                    (lib.set_params)(pcm, SND_PCM_FORMAT_S16_LE, SND_PCM_ACCESS_RW_INTERLEAVED, channels, rate, 1, us);
                if err < 0 {
                    return Err(format!("the sound device won't play: {}", error(lib, err)));
                }
                Ok(p)
            }
        }

        /// Write interleaved stereo samples, waiting for room.
        pub fn write(&mut self, samples: &[i16]) -> Result<(), String> {
            let mut frames = samples;
            while !frames.is_empty() {
                let n = unsafe { (self.lib.writei)(self.pcm, frames.as_ptr().cast(), (frames.len() / 2) as c_ulong) };
                if n < 0 {
                    // Underrun or suspend: recover and carry on.
                    let err = unsafe { (self.lib.recover)(self.pcm, n as c_int, 1) };
                    if err < 0 {
                        return Err(format!("sound stopped: {}", error(self.lib, err)));
                    }
                    continue;
                }
                frames = &frames[(n as usize * 2).min(frames.len())..];
            }
            Ok(())
        }

        /// Sample frames written but not played yet.
        pub fn delay(&self) -> u64 {
            let mut d: c_long = 0;
            let err = unsafe { (self.lib.delay)(self.pcm, &mut d) };
            if err < 0 {
                0
            } else {
                d.max(0) as u64
            }
        }

        /// Stop at once, throwing away what's buffered.
        pub fn stop(&mut self) {
            unsafe { (self.lib.drop)(self.pcm) };
        }
    }

    impl Drop for Pcm {
        fn drop(&mut self) {
            unsafe {
                (self.lib.drop)(self.pcm);
                (self.lib.close)(self.pcm);
            }
        }
    }

    fn error(lib: &Lib, err: c_int) -> String {
        unsafe {
            let s = (lib.strerror)(err);
            if s.is_null() {
                format!("error {err}")
            } else {
                CStr::from_ptr(s).to_string_lossy().into_owned()
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod device {
    //! No sound output on this platform yet: playback is silent.

    use std::time::Duration;

    pub struct Pcm;

    impl Pcm {
        pub fn open(_rate: u32, _channels: u32, _latency: Duration) -> Result<Self, String> {
            Err("no sound on this system yet".into())
        }

        pub fn write(&mut self, _samples: &[i16]) -> Result<(), String> {
            Ok(())
        }

        pub fn delay(&self) -> u64 {
            0
        }

        pub fn stop(&mut self) {}
    }
}
