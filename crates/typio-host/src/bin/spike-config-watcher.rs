//! Phase 2 spike: drive `ConfigWatcher` with a real `calloop` event loop
//! against a live directory, printing each confirmed reload.
//!
//! Run with:
//!
//! ```sh
//! mkdir -p /tmp/typio-spike-cfg
//! cargo run --bin spike-config-watcher -- /tmp/typio-spike-cfg
//! # in another terminal:
//! echo "key = 'value'" > /tmp/typio-spike-cfg/core.toml   # → reload
//! echo "x"             > /tmp/typio-spike-cfg/junk.txt      # → ignored
//! ```
//!
//! The spike exits after 60 seconds or on the first reload, whichever
//! comes first. It demonstrates that:
//!
//! 1. `ConfigWatcher`'s two fds plug cleanly into a `calloop::EventLoop`
//!    via `calloop::generic::Generic`.
//! 2. The debounce state machine produces exactly one reload callback
//!    when a burst of writes lands on `core.toml`.
//! 3. The module is **pure mechanism** — this spike owns the side-effect
//!    (printing); the watcher only decides *when* to fire.

use std::cell::RefCell;
use std::os::fd::BorrowedFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode, PostAction};

use typio_host::config_watcher::ConfigWatcher;

fn main() -> ExitCode {
    let config_dir = match parse_args() {
        Ok(p) => p,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    eprintln!("typio-host Phase 2 spike: watching {config_dir:?}");
    eprintln!("(write to {config_dir:?}/core.toml or platform.toml to fire a reload)");

    let watcher = match ConfigWatcher::with_debounce(&config_dir, Duration::from_millis(100)) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("FAIL: cannot construct watcher: {e}");
            return ExitCode::from(1);
        }
    };

    // The watcher needs to be callable from inside two separate calloop
    // closures. We wrap it in a leaked RefCell — calloop dispatches
    // synchronously on a single thread, so dynamic borrow checking is
    // sufficient. A production daemon would route this through calloop's
    // per-source data slot instead.
    let watcher: &'static RefCell<ConfigWatcher> = Box::leak(Box::new(RefCell::new(watcher)));
    let inotify_fd = watcher.borrow().inotify_fd();
    let timer_fd = watcher.borrow().timer_fd();

    // SAFETY: both fds are owned by the leaked ConfigWatcher which outlives
    // the process. calloop borrows them via BorrowedFd and never closes.
    let inotify_source = unsafe {
        Generic::new(
            BorrowedFd::borrow_raw(inotify_fd),
            Interest::READ,
            Mode::Level,
        )
    };
    let timer_source = unsafe {
        Generic::new(
            BorrowedFd::borrow_raw(timer_fd),
            Interest::READ,
            Mode::Level,
        )
    };

    let mut event_loop: EventLoop<LoopData> = match EventLoop::try_new() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("FAIL: calloop EventLoop::try_new: {e}");
            return ExitCode::from(3);
        }
    };
    let handle = event_loop.handle();

    if let Err(e) = handle.insert_source(inotify_source, |_, _, data: &mut LoopData| {
        let mut w = watcher.borrow_mut();
        let outcome = match w.drain_inotify() {
            Ok(o) => o,
            Err(e) => {
                eprintln!("inotify drain error: {e}");
                return Ok(PostAction::Continue);
            }
        };
        if outcome.should_rearm_watches {
            let _ = w.rearm_watches();
        }
        if outcome.should_schedule_reload {
            let _ = w.schedule_reload();
        }
        data.events_seen += 1;
        Ok(PostAction::Continue)
    }) {
        eprintln!("FAIL: insert inotify source: {e}");
        return ExitCode::from(4);
    }

    if let Err(e) = handle.insert_source(timer_source, |_, _, data: &mut LoopData| {
        let mut w = watcher.borrow_mut();
        match w.drain_timer() {
            Ok(true) => {
                data.reloads_fired += 1;
                eprintln!(
                    "RELOAD #{} (after {} inotify events since last reload)",
                    data.reloads_fired, data.events_seen
                );
                data.events_seen = 0;
            }
            Ok(false) => {} // spurious wakeup
            Err(e) => eprintln!("timer drain error: {e}"),
        }
        Ok(PostAction::Continue)
    }) {
        eprintln!("FAIL: insert timer source: {e}");
        return ExitCode::from(4);
    }

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut data = LoopData::default();
    eprintln!("Running for up to 60s; exits on first reload.");
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(500) as i32;
        match event_loop.dispatch(
            Some(Duration::from_millis(remaining.max(0) as u64)),
            &mut data,
        ) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("dispatch error: {e}");
                return ExitCode::from(5);
            }
        }
        if data.reloads_fired > 0 {
            eprintln!("First reload seen — exiting.");
            return ExitCode::SUCCESS;
        }
    }
    eprintln!("Timeout after 60s with no reload.");
    ExitCode::SUCCESS
}

#[derive(Default, Debug)]
struct LoopData {
    events_seen: usize,
    reloads_fired: usize,
}

fn parse_args() -> Result<PathBuf, String> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().ok_or_else(|| {
        "usage: spike-config-watcher <config-dir>\n\
         \n\
         Watches <config-dir> for changes to core.toml or platform.toml\n\
         and prints a RELOAD line each time a debounced reload fires."
            .to_string()
    })?;
    if dir.starts_with('-') {
        return Err(format!("unknown option: {dir}"));
    }
    if !std::path::Path::new(&dir).is_dir() {
        return Err(format!("not a directory: {dir}"));
    }
    Ok(PathBuf::from(dir))
}
