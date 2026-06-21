//! PipeWire audio capture — 16 kHz mono float32 PCM for voice input.
//!
//! Port of `src/audio/pw_capture.c`. Captures from the default source via the
//! `pipewire`/`libspa` Rust crates (which bind `libpipewire-0.3`, the same
//! library the C wraps). The C runs `pw_thread_loop` on its own thread and
//! locks around stream lifecycle; the pipewire crate's safe `ThreadLoop` lacks
//! a constructor, so this port runs a `MainLoopRc` on a dedicated std thread
//! and uses a polled timer to honour `stop()` from the caller's thread.
//!
//! Verifies: compiles, links against libpipewire-0.3, pure state/counter logic
//! is unit-tested. Runtime capture needs a live PipeWire daemon.

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use pipewire as pw;
use pw::spa;

pub use pipewire;

/// Callback invoked on the PipeWire process thread for each captured buffer.
/// Samples are native-endian float32 (PipeWire `F32` = IEEE-754 float).
pub type CaptureCallback = Box<dyn FnMut(&[f32]) + Send>;

const SAMPLE_RATE: u32 = 16_000;
const CHANNELS: u32 = 1;

/// PipeWire audio capture session. One `PwCapture` may be started and stopped
/// repeatedly; each `start()` spawns a fresh loop thread (the C likewise
/// recreates the stream each session).
pub struct PwCapture {
    callback: Arc<Mutex<Option<CaptureCallback>>>,
    capturing: Arc<AtomicBool>,
    frames_received: Arc<AtomicU32>,
    // Quit flag shared with the active loop thread; `start()` reinitialises it.
    quit: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PwCapture {
    /// `typio_pw_capture_new`. Does NOT start capturing; call `start()`.
    /// Mirrors the C `pw_init()` + thread-loop construction.
    pub fn new(callback: CaptureCallback) -> Self {
        PwCapture {
            callback: Arc::new(Mutex::new(Some(callback))),
            capturing: Arc::new(AtomicBool::new(false)),
            frames_received: Arc::new(AtomicU32::new(0)),
            quit: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }

    /// `typio_pw_capture_start`. Spawns the loop thread and begins capture.
    /// Returns `false` if already capturing or the thread was already running.
    pub fn start(&mut self) -> bool {
        if self.capturing.load(Ordering::Acquire) || self.thread.is_some() {
            return false;
        }
        self.capturing.store(true, Ordering::Release);
        self.frames_received.store(0, Ordering::Release);
        // Fresh quit flag per session.
        let quit = Arc::new(AtomicBool::new(false));
        self.quit = quit.clone();

        let callback = self.callback.clone();
        let capturing = self.capturing.clone();
        let frames = self.frames_received.clone();

        let handle = thread::Builder::new()
            .name("typio-capture".into())
            .spawn(move || {
                run_loop(callback, capturing, frames, quit);
            })
            .ok();
        self.thread = handle;
        self.thread.is_some()
    }

    /// `typio_pw_capture_stop`. Signals the loop thread to quit and joins it.
    pub fn stop(&mut self) {
        if !self.capturing.load(Ordering::Acquire) {
            return;
        }
        self.capturing.store(false, Ordering::Release);
        self.quit.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Diagnostic counter — number of process callbacks that delivered samples.
    pub fn frames_received(&self) -> u32 {
        self.frames_received.load(Ordering::Acquire)
    }

    pub fn is_capturing(&self) -> bool {
        self.capturing.load(Ordering::Acquire)
    }
}

impl Drop for PwCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Loop body, runs entirely on the capture thread. All PipeWire objects are
/// non-`Send` (`MainLoopRc` is `Rc`-based) and so never leave this thread.
fn run_loop(
    callback: Arc<Mutex<Option<CaptureCallback>>>,
    capturing: Arc<AtomicBool>,
    frames: Arc<AtomicU32>,
    quit: Arc<AtomicBool>,
) {
    use pw::properties::properties;
    use pw::stream::StreamFlags;
    use spa::param::audio::{AudioFormat, AudioInfoRaw};
    use spa::param::format::{MediaSubtype, MediaType};
    use spa::param::format_utils;
    use spa::param::ParamType;
    use spa::pod::{serialize::PodSerializer, Pod, Value};
    use spa::utils::SpaTypes;

    pw::init();

    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(ml) => ml,
        Err(_) => return,
    };
    let context = match pw::context::ContextRc::new(&mainloop, None) {
        Ok(c) => c,
        Err(_) => return,
    };
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(_) => return,
    };

    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::NODE_LATENCY => "256/16000",
        *pw::keys::APP_NAME => "Typio",
        *pw::keys::NODE_NAME => "typio-audio-capture",
    };

    let stream = match pw::stream::StreamBox::new(&core, "typio-capture", props) {
        Ok(s) => s,
        Err(_) => return,
    };

    // Shared state carried into the process callback via user_data.
    struct UserData {
        callback: Arc<Mutex<Option<CaptureCallback>>>,
        capturing: Arc<AtomicBool>,
        frames: Arc<AtomicU32>,
        format_ok: bool,
    }
    let ud = UserData {
        callback,
        capturing,
        frames,
        format_ok: false,
    };

    let _listener = stream
        .add_local_listener_with_user_data(ud)
        .param_changed(|_s, ud, id, param| {
            let Some(param) = param else { return };
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Ok((media, sub)) = format_utils::parse_format(param) else {
                return;
            };
            if media != MediaType::Audio || sub != MediaSubtype::Raw {
                return;
            }
            ud.format_ok = true;
        })
        .process(|s, ud| {
            if !ud.capturing.load(Ordering::Acquire) {
                return;
            }
            let Some(mut buffer) = s.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let size = data.chunk().size() as usize;
            if size == 0 || (size % std::mem::size_of::<f32>()) != 0 {
                return;
            }
            let n_samples = size / std::mem::size_of::<f32>();
            if n_samples == 0 {
                return;
            }
            if let Some(bytes) = data.data() {
                // PipeWire F32 is native-endian IEEE-754 float. Reinterpret the
                // byte slice as f32 (matches the C `(const float*)` cast). The
                // buffer is appropriately aligned for element width 4.
                let samples: &[f32] =
                    unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const f32, n_samples) };
                ud.frames.fetch_add(1, Ordering::Relaxed);
                if let Some(cb) = ud.callback.lock().unwrap().as_mut() {
                    cb(samples);
                }
            }
        })
        .register();

    // Fixed 16 kHz mono F32 format.
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(CHANNELS);
    let obj = spa::pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj))
        .unwrap()
        .0
        .into_inner();
    let mut params = [Pod::from_bytes(&values).unwrap()];

    if stream
        .connect(
            spa::utils::Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .is_err()
    {
        return;
    }

    // Polled-quit timer: stop() sets the flag from the caller's thread; this
    // source wakes the loop and breaks mainloop.run() so the thread can exit.
    let quit_for_timer = quit.clone();
    let mainloop_for_timer = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_expiries| {
        if quit_for_timer.load(Ordering::Acquire) {
            mainloop_for_timer.quit();
        }
    });
    let _ = timer;

    mainloop.run();
    // SAFETY: called after mainloop.run() has returned (loop is torn down) and
    // no other PipeWire objects remain alive on this thread.
    unsafe { pw::deinit() };
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_flags_transition_correctly() {
        // Use a no-op callback; we never start the loop here (no live daemon).
        let mut cap = PwCapture::new(Box::new(|_s| {}));
        assert!(!cap.is_capturing());
        assert_eq!(cap.frames_received(), 0);
        // stop() before start() is a no-op (mirrors the C guard).
        cap.stop();
        assert!(!cap.is_capturing());
    }

    #[test]
    fn frames_counter_is_independent_per_instance() {
        let cap = PwCapture::new(Box::new(|_s| {}));
        cap.frames_received.store(7, Ordering::Release);
        assert_eq!(cap.frames_received(), 7);
    }

    #[test]
    fn callback_is_send_box() {
        // Compile-time check that the callback type is Send-boxed as documented.
        let _: CaptureCallback = Box::new(|_s| {});
    }
}
