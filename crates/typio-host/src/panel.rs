//! Candidate panel — flux text rendering on a raw Wayland surface.
//!
//! Uses wayland-sys (raw FFI to libwayland-client) to create a
//! wl_surface independently of wayland-client's safe wrappers. This
//! surface is passed to flux for VkSurfaceKHR creation, bypassing the
//! pointer-isolation problem between wayland-client's opaque Proxy
//! type and flux's need for a raw `wl_surface*`.
//!
//! ## Architecture
//!
//! ```text
//! libwayland-client.so (raw C API via wayland-sys)
//!   → wl_display (shared with wayland-client's Connection)
//!     → wl_compositor → wl_surface
//!       → VkSurfaceKHR (via flux_device + vkCreateWaylandSurfaceKHR)
//!         → flux_surface → flux_canvas → flux_text_draw
//! ```

use std::ffi::c_void;
use std::os::raw::c_char;
use std::ptr;
use std::time::{Duration, Instant};

use flux_sys::{
    flux_arena, flux_arena_destroy, flux_arena_init, flux_arena_reset, flux_canvas,
    flux_canvas_begin, flux_canvas_desc, flux_canvas_destroy, flux_canvas_end,
    flux_canvas_fill_rrect, flux_color_rgba, flux_device, flux_device_create, flux_device_desc,
    flux_device_release, flux_device_vk_instance, flux_error_info, flux_frame, flux_frame_begin_desc,
    flux_frame_present, flux_frame_submit, flux_get_last_error, flux_result, flux_struct_type,
    flux_surface, flux_surface_begin_frame, flux_surface_create, flux_surface_desc, flux_surface_release,
    flux_text, flux_text_create, flux_text_desc, flux_text_destroy, flux_text_draw, flux_text_family,
    flux_text_metrics, flux_text_style,
};
use wayland_sys::{
    client::{wl_proxy, wl_proxy_marshal_array},
    common::wl_argument,
};

use crate::protocols::viewporter::wp_viewport::WpViewport;

use flux_struct_type as FType;
use flux_text_family as FontFamily;

/// Frame-acquire timeout for panel rendering (200 ms). Short enough that
/// a stuck `vkAcquireNextImageKHR` (e.g. popup surface not yet configured
/// by the compositor) fails fast instead of blocking the main loop past
/// the watchdog's 3-second stuck threshold.
const PANEL_FRAME_TIMEOUT_NS: u64 = 200_000_000;
/// Swapchain width quantum (ADR-0013). The buffer is allocated in
/// multiples of this and grows only; sub-quantum widenings reuse the
/// existing swapchain and are cropped to the exact content rect with
/// `wp_viewport`. 64 px is large enough that typical candidate-row
/// width variation stays inside one quantum, so after a short warm-up
/// `flux_surface_resize` (and its `vkDeviceWaitIdle` + WSI roundtrips)
/// is never called again during steady-state paging.
const SURFACE_WIDTH_QUANTUM: u32 = 64;
/// Height quantum (same grow-only logic as width). Banner and
/// candidate rows are both ~40 px at scale 1; a 32 px quantum rounds
/// the first request up to 64 px, fitting inside the pre-allocation
/// in `InputMethodFrontend::connect` and so avoiding a first-render
/// `flux_surface_resize` (which blocks the loop on the compositor's
/// swapchain release and trips the 3 s watchdog on the very first
/// indicator banner). Without this, height was exact-matched while
/// width was grow-only — a quiet asymmetry that made every panel
/// flush in a new process pay a full swapchain recreate.
const SURFACE_HEIGHT_QUANTUM: u32 = 32;
/// Initial swapchain size passed to `FluxPanel::new_from_surface` by
/// `InputMethodFrontend::connect`. Sized to skip the first automatic
/// indicator banner's `flux_surface_resize` (and its `vkDeviceWaitIdle`
/// + compositor swapchain release) at the common display scales, where
/// the watchdog is armed but unforgiving — see the audit after the
/// 0a91080 height-quantisation fix that still tripped the watchdog on
/// width at scale 2.
///
/// Banner geometry for the longest observed default indicator label
/// `"中 · Rime · 懿拼音"` (text metric 119.3 px logical, plus
/// `2 * BANNER_PADDING = 20`):
///
/// | scale | phys (W×H) | quantised (W×H) |
/// |------:|-----------|-----------------|
/// |  1.0  | 140 × 40  | 192 × 64        |
/// |  1.5  | 210 × 60  | 256 × 64        |
/// |  2.0  | 280 × 80  | 320 × 96        |
/// |  3.0  | 420 × 120 | 448 × 128       |
///
/// `512 × 128` covers every cell in the table with one width-quantum
/// of headroom (512 − 448 = 64). Larger indicator labels (long engine
/// names, verbose mode displays) and scales ≥ 4 still trigger a
/// one-time resize — but only after the user has actually started
/// typing or switched engine, by which point the watchdog tolerance
/// has been replaced by genuine interaction cadence.
///
/// `PANEL_PREALLOC_WIDTH` is a multiple of `SURFACE_WIDTH_QUANTUM`,
/// `PANEL_PREALLOC_HEIGHT` of `SURFACE_HEIGHT_QUANTUM`; this keeps the
/// initial allocation on a quantum boundary so the first grow-only
/// decision in `apply_grow_only_size` is a no-op when the content fits.
pub const PANEL_PREALLOC_WIDTH: u32 = 512;
pub const PANEL_PREALLOC_HEIGHT: u32 = 128;
const PANEL_PADDING: f32 = 8.0;
const PANEL_ROW_HEIGHT: f32 = 24.0;
const CANDIDATE_FONT_SIZE: f32 = 16.0;
const CANDIDATE_ITEM_X_PADDING: f32 = 5.0;
const CANDIDATE_ITEM_GAP: f32 = 8.0;
const CANDIDATE_NUMBER_FONT_SIZE: f32 = 11.0;
const CANDIDATE_NUMBER_GAP: f32 = 4.0;
/// Status-banner (indicator / voice) layout constants. Kept separate from
/// the candidate-row metrics above: the banner is a single centred text
/// segment, not a two-column "number + candidate" row, so its padding and
/// font size are tuned independently. The same VkSurface / flux_text /
/// atlas stack is shared (ADR-0017 — one popup surface, mutually exclusive
/// owners).
const BANNER_PADDING: f32 = 10.0;
const BANNER_FONT_SIZE: f32 = 15.0;
/// Computed banner row height: padding above + font box + padding below.
/// `1.3` is the typical line-height factor flux applies for default fonts.
const BANNER_ROW_HEIGHT: f32 = BANNER_PADDING * 2.0 + BANNER_FONT_SIZE * 1.3;

const PANEL_TIMING_TARGET: &str = "typio.panel.timing";
const PANEL_TIMING_SLOW_THRESHOLD: Duration = Duration::from_millis(12);

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[derive(Clone, Copy, Debug, Default)]
struct TextStatsSnapshot {
    glyph_count: u64,
    glyph_cap: u64,
    glyph_hits: u64,
    glyph_misses: u64,
    glyph_evictions: u64,
    atlas_clears: u64,
}

unsafe fn text_stats_snapshot(text: *mut flux_text) -> TextStatsSnapshot {
    let mut stats: flux_sys::flux_text_stats = unsafe { std::mem::zeroed() };
    unsafe {
        flux_sys::flux_text_get_stats(text, &mut stats);
    }
    TextStatsSnapshot {
        glyph_count: stats.glyph_count as u64,
        glyph_cap: stats.glyph_cap as u64,
        glyph_hits: stats.glyph_hits,
        glyph_misses: stats.glyph_misses,
        glyph_evictions: stats.glyph_evictions,
        atlas_clears: stats.atlas_clears,
    }
}

// ── Pipeline-cache persistence ────────────────────────────────────────
//
// flux's new pipeline-cache API (Skia PersistentCache model) lets the
// consumer own the storage strategy via load/save callbacks on
// flux_device_desc. We persist to $XDG_CACHE_HOME/typio/pipeline.bin
// (or $HOME/.cache/typio/pipeline.bin) so shader compilation cost is
// paid once; subsequent daemon starts reuse the cached VkPipelineCache
// blob. The load callback returns a libc::malloc'd buffer (flux frees
// it with C free()); the save callback writes atomically (temp +
// rename). Both are best-effort and silent on failure — the cache is
// an optimisation, not a correctness path.

/// Resolve the pipeline-cache path following XDG conventions.
/// Returns None when no cache home is available.
fn pipeline_cache_path() -> Option<std::path::PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return Some(std::path::PathBuf::from(xdg).join("typio/pipeline.bin"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return Some(std::path::PathBuf::from(home).join(".cache/typio/pipeline.bin"));
        }
    }
    None
}

unsafe extern "C" fn pipeline_cache_load(
    userdata: *mut c_void,
    out_size: *mut usize,
) -> *mut c_void {
    if userdata.is_null() || out_size.is_null() {
        return ptr::null_mut();
    }
    let path = std::ffi::CStr::from_ptr(userdata as *const c_char);
    let path = match path.to_str() {
        Ok(s) => std::path::Path::new(s),
        Err(_) => return ptr::null_mut(),
    };
    match std::fs::read(path) {
        Ok(data) => {
            let size = data.len();
            let buf = libc::malloc(size);
            if buf.is_null() {
                return ptr::null_mut();
            }
            ptr::copy_nonoverlapping(data.as_ptr(), buf as *mut u8, size);
            *out_size = size;
            buf
        }
        Err(_) => {
            *out_size = 0;
            ptr::null_mut()
        }
    }
}

unsafe extern "C" fn pipeline_cache_save(userdata: *mut c_void, data: *const c_void, size: usize) {
    if userdata.is_null() || data.is_null() || size == 0 {
        return;
    }
    let path = std::ffi::CStr::from_ptr(userdata as *const c_char);
    let path = match path.to_str() {
        Ok(s) => s.to_owned(),
        Err(_) => return,
    };
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let bytes = std::slice::from_raw_parts(data as *const u8, size);
    let tmp = format!("{path}.tmp.{pid}", pid = std::process::id());
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn free_cache_path(p: *mut c_char) {
    if !p.is_null() {
        unsafe {
            let _ = std::ffi::CString::from_raw(p);
        }
    }
}

/// A candidate panel backed by flux on a raw Wayland surface.
///
/// Created from a raw `wl_display*` (obtainable from
/// `Connection::backend().display_ptr()`). The panel creates its own
/// wl_surface via the raw C API, uses it for VkSurfaceKHR, and
/// renders candidates via flux_text_draw.
pub struct FluxPanel {
    device: *mut flux_device,
    surface: *mut flux_surface,
    canvas: *mut flux_canvas,
    text: *mut flux_text,
    arena: flux_arena,
    width: u32,
    height: u32,
    scale: f32,
    /// `wp_viewport` on the panel surface, used to crop the grow-only
    /// swapchain to the exact content rect (ADR-0013). `None` only when
    /// the compositor lacks `wp_viewporter`; in that case the swapchain
    /// is sized exactly to the content and the per-page resize cost
    /// returns.
    viewport: Option<WpViewport>,
    /// Last content width (logical, pre-scale) sent to `wp_viewport`.
    /// Tracked so we only re-issue set_source/set_destination when the
    /// visible size actually changes, not on every redraw.
    content_w_logical: i32,
    content_h_logical: i32,
    last_candidate_size_duration: Duration,
    last_candidate_size_resized: bool,
    // Keep the raw wl_surface alive for the panel's lifetime.
    _wl_surface: *mut c_void,
    /// Heap-allocated C string for the pipeline-cache path. Owned by
    /// FluxPanel so it outlives `flux_device_release` (which fires the
    /// save callback). Null when cache persistence is unavailable
    /// (no XDG_CACHE_HOME / HOME).
    pipeline_cache_path: *mut c_char,
    /// Cached per-candidate layout: `(number_metrics, text_metrics)`
    /// for each entry in `last_layout_key.0`, in logical pixels.
    /// Recomputed only when the candidate strings or `scale` change.
    /// Both `ensure_candidate_size` (total-width sizing) and
    /// `draw_candidates` (per-item placement) consult this cache, so a
    /// candidate-highlight-only update — the canonical Up/Down arrow
    /// navigation case — skips the `flux_text_measure` FFI loop
    /// entirely.
    last_layout_key: Option<(Vec<String>, f32)>,
    last_layout: Vec<(flux_text_metrics, flux_text_metrics)>,
}

impl FluxPanel {
    /// Create a panel backed by a Wayland surface via Vulkan.
    ///
    /// `wl_display_ptr` must be a valid `*mut wl_display` obtained
    /// from the same Wayland connection the input-method frontend
    /// uses (via `Connection::backend().display_ptr()`).
    ///
    /// `viewport`, when present, attaches a `wp_viewport` to the
    /// surface so the swapchain can be allocated grow-only and cropped
    /// to exact content (ADR-0013). `None` falls back to exact-size
    /// resize.
    ///
    /// # Safety
    /// `wl_display_ptr` must be valid for the panel's lifetime.
    /// Create a panel backed by an EXISTING wl_surface (e.g. one that's
    /// already connected to zwp_input_popup_surface_v2 for positioning).
    /// The surface must outlive the panel.
    pub unsafe fn new_from_surface(
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
        viewport: Option<WpViewport>,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        Self::new_inner(wl_display_ptr, wl_surface_ptr, viewport, width, height)
    }

    fn new_inner(
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
        viewport: Option<WpViewport>,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        unsafe { Self::new_inner_unsafe(wl_display_ptr, wl_surface_ptr, viewport, width, height) }
    }

    unsafe fn new_inner_unsafe(
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
        viewport: Option<WpViewport>,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        // 1. Use the provided wl_surface directly.
        let wl_surface = wl_surface_ptr;
        if wl_surface.is_null() {
            return Err("wl_surface is null".into());
        }

        // 2. Create Vulkan device with Wayland surface + swapchain extensions.
        let instance_exts: [*const c_char; 2] = [
            c"VK_KHR_surface".as_ptr(),
            c"VK_KHR_wayland_surface".as_ptr(),
        ];
        let device_exts: [*const c_char; 1] = [c"VK_KHR_swapchain".as_ptr()];
        // Resolve pipeline-cache path before creating the desc so the
        // load/save callbacks and userdata can be wired in.
        let cache_path_c: *mut c_char = match &pipeline_cache_path() {
            Some(path) => {
                let cstring = std::ffi::CString::new(path.to_string_lossy().as_ref())
                    .map_err(|e| format!("cache path has NUL: {e}"))?;
                cstring.into_raw()
            }
            None => ptr::null_mut(),
        };
        let mut device_desc: flux_device_desc = std::mem::zeroed();
        device_desc.type_ = FType::FLUX_TYPE_DEVICE_DESC;
        device_desc.required_instance_extensions = instance_exts.as_ptr();
        device_desc.required_instance_extension_count = instance_exts.len() as u32;
        device_desc.required_device_extensions = device_exts.as_ptr();
        device_desc.required_device_extension_count = device_exts.len() as u32;
        device_desc.frames_in_flight = 2;
        if !cache_path_c.is_null() {
            device_desc.pipeline_cache_load = Some(pipeline_cache_load);
            device_desc.pipeline_cache_save = Some(pipeline_cache_save);
            device_desc.pipeline_cache_userdata = cache_path_c as *mut c_void;
        }

        let mut device: *mut flux_device = ptr::null_mut();
        let r = flux_device_create(&device_desc, &mut device);
        if !flux_result_is_ok(r) {
            free_cache_path(cache_path_c);
            return Err(flux_last_error_string("flux_device_create"));
        }

        // 3. Create VkSurfaceKHR from the wl_surface.
        let vk_instance = flux_device_vk_instance(device) as *mut c_void;
        let vk_surface = create_wayland_vk_surface(vk_instance, wl_display_ptr, wl_surface)?;
        if vk_surface.is_null() {
            flux_device_release(device);
            free_cache_path(cache_path_c);
            return Err("vkCreateWaylandSurfaceKHR returned NULL".into());
        }

        // 4. Create flux surface.
        let mut surface_desc: flux_surface_desc = std::mem::zeroed();
        surface_desc.type_ = FType::FLUX_TYPE_SURFACE_DESC;
        surface_desc.vk_surface_khr = vk_surface;
        surface_desc.width = width;
        surface_desc.height = height;

        let mut surface: *mut flux_surface = ptr::null_mut();
        let r = flux_surface_create(device, &surface_desc, &mut surface);
        if !flux_result_is_ok(r) {
            flux_device_release(device);
            free_cache_path(cache_path_c);
            return Err(flux_last_error_string("flux_surface_create"));
        }

        // 5. Create flux canvas.
        let mut canvas_desc: flux_canvas_desc = std::mem::zeroed();
        canvas_desc.type_ = FType::FLUX_TYPE_CANVAS_DESC;
        canvas_desc.surface = surface;
        canvas_desc.scale = 1.0;

        let mut canvas: *mut flux_canvas = ptr::null_mut();
        let r = flux_sys::flux_canvas_create(&canvas_desc, &mut canvas);
        if !flux_result_is_ok(r) {
            flux_surface_release(surface);
            flux_device_release(device);
            free_cache_path(cache_path_c);
            return Err(flux_last_error_string("flux_canvas_create"));
        }

        // 6. Create flux text context.
        let mut text_desc: flux_text_desc = std::mem::zeroed();
        text_desc.device = device;
        text_desc.scale = 1.0;

        let mut text: *mut flux_text = ptr::null_mut();
        let r = flux_text_create(&text_desc, &mut text);
        if !flux_result_is_ok(r) {
            flux_sys::flux_canvas_destroy(canvas);
            flux_surface_release(surface);
            flux_device_release(device);
            free_cache_path(cache_path_c);
            return Err(flux_last_error_string("flux_text_create"));
        }

        // 7. Create arena for per-frame text shaping allocations.
        let mut arena: flux_arena = unsafe { std::mem::zeroed() };
        let r = flux_arena_init(&mut arena, 256 * 1024, ptr::null_mut());
        if !flux_result_is_ok(r) {
            flux_text_destroy(text);
            flux_sys::flux_canvas_destroy(canvas);
            flux_surface_release(surface);
            flux_device_release(device);
            free_cache_path(cache_path_c);
            return Err(flux_last_error_string("flux_arena_init"));
        }

        Ok(Self {
            device,
            surface,
            canvas,
            text,
            arena,
            width,
            height,
            scale: 1.0,
            viewport,
            content_w_logical: 0,
            content_h_logical: 0,
            last_candidate_size_duration: Duration::ZERO,
            last_candidate_size_resized: false,
            _wl_surface: wl_surface,
            pipeline_cache_path: cache_path_c,
            last_layout_key: None,
            last_layout: Vec::new(),
        })
    }

    /// Set the HiDPI scale factor for rendering.
    pub fn set_scale(&mut self, scale: f32) {
        if (self.scale - scale).abs() < 0.01 {
            return;
        }
        self.scale = scale;
        unsafe {
            flux_sys::flux_canvas_set_scale(self.canvas, scale);
            flux_sys::flux_text_set_scale(self.text, scale);
        }
    }

    /// Draw candidate strings with the selected one highlighted.
    ///
    /// `heartbeat` is invoked between each blocking FFI call
    /// (`flux_surface_begin_frame`, `flux_frame_submit`,
    /// `flux_frame_present`, …) so the daemon's watchdog sees
    /// progress even when a single call blocks past the 3-second
    /// stuck threshold — the documented failure mode under rapid
    /// candidate cycling, where the Wayland compositor cannot
    /// release swapchain images as fast as the engine emits
    /// composition callbacks and `vkQueuePresentKHR`/`vkAcquireNext-
    /// ImageKHR` stall waiting for them. Heartbeating between
    /// sub-steps distinguishes "slow but progressing" from a true
    /// hang; a genuinely deadlocked FFI call stops heartbeating too
    /// and the watchdog still fires. Callers pass `&|| wd!().heart-
    /// beat()` from the loop; tests pass `&|| {}`.
    ///
    /// `before_present` is invoked immediately before
    /// `flux_frame_present` — the one FFI call that can block
    /// inside the WSI/driver for several seconds under compositor
    /// back-pressure with no opportunity to heartbeat from the same
    /// thread. Callers set the watchdog stage to `Present` so the
    /// per-stage threshold (15 s) tolerates the transient stall
    /// instead of SIGKILLing a recovering panel. Tests pass `&|| {}`.
    pub fn draw_candidates(
        &mut self,
        candidates: &[String],
        selected: usize,
        composition_seq: u64,
        heartbeat: &dyn Fn(),
        before_present: &dyn Fn(),
    ) {
        let timing_trace_enabled =
            tracing::enabled!(target: PANEL_TIMING_TARGET, tracing::Level::TRACE);
        let timing_info_enabled =
            tracing::enabled!(target: PANEL_TIMING_TARGET, tracing::Level::INFO);
        let timing_enabled = timing_trace_enabled || timing_info_enabled;
        let total_start = timing_enabled.then(Instant::now);
        let frame_id = if timing_enabled {
            use std::sync::atomic::{AtomicU64, Ordering};
            static FRAME_ID: AtomicU64 = AtomicU64::new(1);
            Some(FRAME_ID.fetch_add(1, Ordering::Relaxed))
        } else {
            None
        };
        let stats_before = if timing_enabled {
            Some(unsafe { text_stats_snapshot(self.text) })
        } else {
            None
        };
        let mut begin_frame_duration = Duration::ZERO;
        let mut canvas_begin_duration = Duration::ZERO;
        let mut measure_duration = Duration::ZERO;
        let mut draw_duration = Duration::ZERO;
        let mut submit_duration = Duration::ZERO;
        let mut present_duration = Duration::ZERO;

        macro_rules! timed {
            ($slot:ident, $expr:expr) => {{
                if timing_enabled {
                    let start = Instant::now();
                    let value = $expr;
                    $slot += start.elapsed();
                    value
                } else {
                    $expr
                }
            }};
        }

        heartbeat();
        // Resolve per-candidate metrics up front via the shared layout
        // cache. The cache hit path (canonical Up/Down arrow case: same
        // candidate strings, only `selected` moved) skips the
        // `flux_text_measure` FFI loop entirely. Done outside the
        // `unsafe` block because `layout_candidates` takes `&mut self`
        // and so cannot share the borrow with the FFI calls below.
        let layout = timed!(measure_duration, self.layout_candidates(candidates));
        unsafe {
            flux_arena_reset(&mut self.arena);
            heartbeat();

            let frame_desc = flux_frame_begin_desc {
                type_: FType::FLUX_TYPE_FRAME_BEGIN_DESC,
                next: ptr::null(),
                timeout_ns: PANEL_FRAME_TIMEOUT_NS,
            };
            let mut frame: *mut flux_frame = ptr::null_mut();
            let r = timed!(
                begin_frame_duration,
                flux_surface_begin_frame(self.surface, &frame_desc, &mut frame)
            );
            heartbeat();
            if !flux_result_is_ok(r) {
                return;
            }

            let bg = flux_color_rgba(28, 28, 32, 255);
            let r = timed!(
                canvas_begin_duration,
                flux_canvas_begin(self.canvas, frame, &bg)
            );
            heartbeat();
            if !flux_result_is_ok(r) {
                return;
            }

            let text_color = flux_color_rgba(240, 240, 240, 255);
            let number_color = flux_color_rgba(145, 145, 152, 255);
            let highlight = flux_color_rgba(56, 84, 160, 255);

            let style = flux_text_style {
                size_px: CANDIDATE_FONT_SIZE,
                weight: 400.0,
                color: text_color,
                family: FontFamily::FLUX_TEXT_FAMILY_DEFAULT,
            };
            let number_style = flux_text_style {
                size_px: CANDIDATE_NUMBER_FONT_SIZE,
                weight: 400.0,
                color: number_color,
                family: FontFamily::FLUX_TEXT_FAMILY_DEFAULT,
            };

            let mut current_x = PANEL_PADDING;
            let y = PANEL_PADDING;

            for (i, candidate) in candidates.iter().enumerate() {
                let number = candidate_number_label(i);
                let number_bytes = number.as_bytes();
                let bytes = candidate.as_bytes();
                let (number_metrics, metrics) = layout[i];

                let item_width = CANDIDATE_ITEM_X_PADDING * 2.0
                    + number_metrics.width
                    + CANDIDATE_NUMBER_GAP
                    + metrics.width;
                let text_top = y + (PANEL_ROW_HEIGHT - metrics.height).max(0.0) / 2.0;
                let number_top = text_top + metrics.baseline - number_metrics.baseline;

                if i == selected {
                    timed!(
                        draw_duration,
                        flux_canvas_fill_rrect(
                            self.canvas,
                            flux_sys::flux_rect {
                                x: current_x,
                                y,
                                w: item_width,
                                h: PANEL_ROW_HEIGHT,
                            },
                            4.0,
                            highlight,
                        )
                    );
                }

                timed!(
                    draw_duration,
                    flux_text_draw(
                        self.text,
                        self.canvas,
                        &mut self.arena,
                        current_x + CANDIDATE_ITEM_X_PADDING,
                        number_top,
                        number_bytes.as_ptr() as *const _,
                        number_bytes.len(),
                        &number_style,
                    )
                );
                timed!(
                    draw_duration,
                    flux_text_draw(
                        self.text,
                        self.canvas,
                        &mut self.arena,
                        current_x
                            + CANDIDATE_ITEM_X_PADDING
                            + number_metrics.width
                            + CANDIDATE_NUMBER_GAP,
                        text_top,
                        bytes.as_ptr() as *const _,
                        bytes.len(),
                        &style,
                    )
                );

                current_x += item_width + CANDIDATE_ITEM_GAP;
            }
            heartbeat();

            flux_canvas_end(self.canvas);
            heartbeat();
            let r = timed!(submit_duration, flux_frame_submit(frame));
            heartbeat();
            if !flux_result_is_ok(r) {
                return;
            }
            before_present();
            timed!(present_duration, flux_frame_present(frame));
            heartbeat();
            self.log_text_stats();
        }
        if let Some(total_start) = total_start {
            let total_duration = total_start.elapsed();
            let slow = total_duration >= PANEL_TIMING_SLOW_THRESHOLD;
            if slow || timing_trace_enabled {
                let stats_after = unsafe { text_stats_snapshot(self.text) };
                let stats_before = stats_before.unwrap_or_default();
                macro_rules! emit_panel_timing {
                    ($level:ident) => {
                        tracing::$level!(
                            target: PANEL_TIMING_TARGET,
                            frame_id = frame_id.unwrap_or(0),
                            composition_seq,
                            candidate_count = candidates.len(),
                            selected,
                            slow,
                            slow_threshold_ms = ms(PANEL_TIMING_SLOW_THRESHOLD),
                            total_ms = ms(total_duration),
                            size_ms = ms(self.last_candidate_size_duration),
                            resized = self.last_candidate_size_resized,
                            begin_ms = ms(begin_frame_duration),
                            canvas_ms = ms(canvas_begin_duration),
                            measure_ms = ms(measure_duration),
                            draw_ms = ms(draw_duration),
                            submit_ms = ms(submit_duration),
                            present_ms = ms(present_duration),
                            glyph_count = stats_after.glyph_count,
                            glyph_cap = stats_after.glyph_cap,
                            glyph_hits_delta = stats_after
                                .glyph_hits
                                .saturating_sub(stats_before.glyph_hits),
                            glyph_misses_delta = stats_after
                                .glyph_misses
                                .saturating_sub(stats_before.glyph_misses),
                            glyph_evictions_delta = stats_after
                                .glyph_evictions
                                .saturating_sub(stats_before.glyph_evictions),
                            atlas_clears_delta = stats_after
                                .atlas_clears
                                .saturating_sub(stats_before.atlas_clears),
                            surface_width = self.width,
                            surface_height = self.height,
                            has_viewport = self.viewport.is_some(),
                            "panel frame"
                        );
                    };
                }
                if slow {
                    emit_panel_timing!(info);
                } else {
                    emit_panel_timing!(trace);
                }
            }
        }
    }

    /// Emit glyph-cache + atlas stats to stderr. Throttled to once every
    /// 120 candidate frames, or immediately when `atlas_clears` rises
    /// (the signal that the atlas saturated and the next frame must
    /// re-rasterise every visible glyph).
    fn log_text_stats(&self) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static FRAME: AtomicU64 = AtomicU64::new(0);
        static LAST_CLEARS: AtomicU64 = AtomicU64::new(0);
        let n = FRAME.fetch_add(1, Ordering::Relaxed);
        let mut stats: flux_sys::flux_text_stats = unsafe { std::mem::zeroed() };
        unsafe {
            flux_sys::flux_text_get_stats(self.text, &mut stats);
        }
        let clears = stats.atlas_clears;
        let last = LAST_CLEARS.load(Ordering::Relaxed);
        if clears != last || n % 120 == 0 {
            LAST_CLEARS.store(clears, Ordering::Relaxed);
            tracing::debug!(
                target: "typio.panel.text",
                frame = n,
                glyph_count = stats.glyph_count,
                glyph_max_cap = stats.glyph_max_cap,
                glyph_cap = stats.glyph_cap,
                glyph_hits = stats.glyph_hits,
                glyph_misses = stats.glyph_misses,
                glyph_evictions = stats.glyph_evictions,
                glyph_invalidations = stats.glyph_invalidations,
                glyph_grows = stats.glyph_grows,
                atlas_clears = stats.atlas_clears,
                "flux text stats"
            );
        }
    }

    /// Resize the panel surface.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let changed = self.width != width || self.height != height;
        self.width = width;
        self.height = height;
        if changed && !self.surface.is_null() {
            unsafe {
                flux_sys::flux_surface_resize(self.surface, width, height);
            }
        }
    }

    /// Return cached `(number_metrics, text_metrics)` per candidate,
    /// re-running `flux_text_measure` only when the candidate strings or
    /// the rendering scale have changed since the last call.
    ///
    /// Both [`Self::ensure_candidate_size`] (total-width sizing) and
    /// [`Self::draw_candidates`] (per-item placement) consult this cache,
    /// so a highlight-only update — the canonical Up/Down arrow case,
    /// where the candidate set is unchanged and only `selected` differs —
    /// skips the `flux_text_measure` FFI loop entirely.
    fn layout_candidates(
        &mut self,
        candidates: &[String],
    ) -> Vec<(flux_text_metrics, flux_text_metrics)> {
        let cache_valid = self
            .last_layout_key
            .as_ref()
            .map(|(cached, scale)| {
                *scale == self.scale && cached.len() == candidates.len() && {
                    cached.iter().zip(candidates.iter()).all(|(a, b)| a == b)
                }
            })
            .unwrap_or(false);
        if cache_valid {
            return self.last_layout.clone();
        }

        let style = flux_sys::flux_text_style {
            size_px: CANDIDATE_FONT_SIZE,
            weight: 400.0,
            color: unsafe { flux_sys::flux_color_rgba(240, 240, 240, 255) },
            family: FontFamily::FLUX_TEXT_FAMILY_DEFAULT,
        };
        let number_style = flux_sys::flux_text_style {
            size_px: CANDIDATE_NUMBER_FONT_SIZE,
            weight: 400.0,
            color: unsafe { flux_sys::flux_color_rgba(145, 145, 152, 255) },
            family: FontFamily::FLUX_TEXT_FAMILY_DEFAULT,
        };

        let mut out: Vec<(flux_text_metrics, flux_text_metrics)> =
            Vec::with_capacity(candidates.len());
        for (i, candidate) in candidates.iter().enumerate() {
            let number = candidate_number_label(i);
            let number_bytes = number.as_bytes();
            let number_metrics = unsafe {
                flux_sys::flux_text_measure(
                    self.text,
                    number_bytes.as_ptr() as *const _,
                    number_bytes.len(),
                    &number_style,
                )
            };
            let bytes = candidate.as_bytes();
            let metrics = unsafe {
                flux_sys::flux_text_measure(
                    self.text,
                    bytes.as_ptr() as *const _,
                    bytes.len(),
                    &style,
                )
            };
            out.push((number_metrics, metrics));
        }
        self.last_layout_key = Some((candidates.to_vec(), self.scale));
        self.last_layout = out.clone();
        out
    }

    /// Ensure the surface is big enough for `candidate_count` rows.
    ///
    /// Two paths, per ADR-0013:
    ///
    /// - **With `wp_viewport` (preferred).** The swapchain buffer is
    ///   quantised up to `SURFACE_WIDTH_QUANTUM` and grows only. A width
    ///   change inside the current quantum reuses the existing swapchain
    ///   — no `vkDeviceWaitIdle`, no WSI roundtrips — and the exact
    ///   content rect is cropped via `wp_viewport.set_source` /
    ///   `set_destination`. After a short warm-up the buffer reaches the
    ///   widest candidate row and `flux_surface_resize` is never called
    ///   again during steady-state paging.
    ///
    /// - **Without `wp_viewport` (legacy).** The buffer must equal the
    ///   content exactly (the buffer maps 1:1 to the surface), so any
    ///   width change rebuilds the swapchain. This is the watchdog-killing
    ///   path when candidate pages churn; viewporter is the fix.
    pub fn ensure_candidate_size(&mut self, candidates: &[String]) {
        let timing_enabled = tracing::enabled!(target: PANEL_TIMING_TARGET, tracing::Level::INFO)
            || tracing::enabled!(target: PANEL_TIMING_TARGET, tracing::Level::TRACE);
        let start = timing_enabled.then(Instant::now);

        // Hit the shared layout cache so the canonical arrow-key
        // navigation case (same candidates, only `selected` moved)
        // skips the `flux_text_measure` FFI loop entirely.
        let layout = self.layout_candidates(candidates);
        let mut total_width: f32 = PANEL_PADDING;
        for (number_metrics, metrics) in layout.iter() {
            let item_width = CANDIDATE_ITEM_X_PADDING * 2.0
                + number_metrics.width
                + CANDIDATE_NUMBER_GAP
                + metrics.width;
            total_width += item_width + CANDIDATE_ITEM_GAP;
        }

        if !candidates.is_empty() {
            total_width = total_width - CANDIDATE_ITEM_GAP + PANEL_PADDING;
        } else {
            total_width += PANEL_PADDING;
        }

        let desired_width = (total_width as u32).max(10);
        let desired_height = (PANEL_PADDING * 2.0 + PANEL_ROW_HEIGHT).ceil() as u32;

        let phys_width = (desired_width as f32 * self.scale).ceil() as u32;
        let phys_height = (desired_height as f32 * self.scale).ceil() as u32;

        let content_w_logical = desired_width as i32;
        let content_h_logical = desired_height as i32;

        self.last_candidate_size_resized = self.apply_grow_only_size(
            phys_width,
            phys_height,
            content_w_logical,
            content_h_logical,
        );
        if let Some(start) = start {
            self.last_candidate_size_duration = start.elapsed();
        }
    }

    /// Grow-only swapchain sizing shared by the candidate and banner
    /// paths (ADR-0013, extended to height). Both axes are quantised
    /// up and never shrink, so any content change that stays inside
    /// the current quantum reuses the existing swapchain and only
    /// re-issues a `wp_viewport` crop. This keeps `flux_surface_resize`
    /// — and its `vkDeviceWaitIdle` + WSI roundtrip — out of the
    /// steady-state render path, including the first indicator banner
    /// of every fresh daemon (where the watchdog is unforgiving).
    ///
    /// Falls back to exact-size `resize()` when the compositor lacks
    /// `wp_viewporter`; that path rebuilds the swapchain on every
    /// change, which ADR-0013 documented as the watchdog-killing case.
    fn apply_grow_only_size(
        &mut self,
        phys_width: u32,
        phys_height: u32,
        content_w_logical: i32,
        content_h_logical: i32,
    ) -> bool {
        if let Some(viewport) = self.viewport.as_ref() {
            let mut resized = false;
            let quantised_phys_w =
                phys_width.div_ceil(SURFACE_WIDTH_QUANTUM) * SURFACE_WIDTH_QUANTUM;
            let quantised_phys_h =
                phys_height.div_ceil(SURFACE_HEIGHT_QUANTUM) * SURFACE_HEIGHT_QUANTUM;
            let target_phys_w = self.width.max(quantised_phys_w);
            let target_phys_h = self.height.max(quantised_phys_h);
            if target_phys_w != self.width || target_phys_h != self.height {
                resized = true;
                self.width = target_phys_w;
                self.height = target_phys_h;
                if !self.surface.is_null() {
                    unsafe {
                        flux_sys::flux_surface_resize(self.surface, target_phys_w, target_phys_h);
                    }
                }
            }
            // Always re-issue the crop so the compositor shows the exact
            // content rect regardless of buffer size. Cheap: two protocol
            // requests, applied at the next commit (i.e. the present in
            // draw_candidates / draw_status_banner or the detach in hide).
            if content_w_logical != self.content_w_logical
                || content_h_logical != self.content_h_logical
            {
                viewport.set_source(0.0, 0.0, content_w_logical as f64, content_h_logical as f64);
                viewport.set_destination(content_w_logical, content_h_logical);
                self.content_w_logical = content_w_logical;
                self.content_h_logical = content_h_logical;
            }
            resized
        } else {
            // Legacy path: buffer must equal content exactly.
            let resized = self.width != phys_width || self.height != phys_height;
            self.resize(phys_width, phys_height);
            resized
        }
    }

    /// Hide the panel by detaching the current Wayland buffer.
    pub fn hide(&mut self) {
        unsafe {
            wl_surface_detach_and_commit(self._wl_surface);
        }
    }

    /// Draw the status banner — a single centred text label used by the
    /// indicator (engine · mode feedback) and voice status overlays. Shares
    /// the candidate panel's VkSurface and flux text stack per ADR-0017
    /// (one positioned popup surface, mutually exclusive owners).
    ///
    /// Empty labels are ignored — caller should `hide()` instead.
    ///
    /// `heartbeat` mirrors [`FluxPanel::draw_candidates`]: invoked
    /// between blocking FFI calls so a slow compositor does not trip
    /// the watchdog. `before_present` is invoked immediately before
    /// `flux_frame_present` so the caller can transition the watchdog
    /// to the longer-threshold `Present` stage; see
    /// [`FluxPanel::draw_candidates`] for the rationale.
    pub fn draw_status_banner(
        &mut self,
        label: &str,
        heartbeat: &dyn Fn(),
        before_present: &dyn Fn(),
    ) {
        if label.is_empty() {
            return;
        }
        heartbeat();
        unsafe {
            flux_arena_reset(&mut self.arena);
            heartbeat();

            let frame_desc = flux_frame_begin_desc {
                type_: FType::FLUX_TYPE_FRAME_BEGIN_DESC,
                next: ptr::null(),
                timeout_ns: PANEL_FRAME_TIMEOUT_NS,
            };
            let mut frame: *mut flux_frame = ptr::null_mut();
            let r = flux_surface_begin_frame(self.surface, &frame_desc, &mut frame);
            heartbeat();
            if !flux_result_is_ok(r) {
                return;
            }

            let bg = flux_color_rgba(28, 28, 32, 255);
            let r = flux_canvas_begin(self.canvas, frame, &bg);
            heartbeat();
            if !flux_result_is_ok(r) {
                return;
            }

            let text_color = flux_color_rgba(240, 240, 240, 255);
            let style = flux_text_style {
                size_px: BANNER_FONT_SIZE,
                weight: 400.0,
                color: text_color,
                family: FontFamily::FLUX_TEXT_FAMILY_DEFAULT,
            };

            let bytes = label.as_bytes();
            let metrics = flux_sys::flux_text_measure(
                self.text,
                bytes.as_ptr() as *const _,
                bytes.len(),
                &style,
            );
            heartbeat();

            let text_y = BANNER_PADDING + (BANNER_FONT_SIZE * 1.3 - metrics.height).max(0.0) / 2.0;
            let text_x = BANNER_PADDING;

            flux_text_draw(
                self.text,
                self.canvas,
                &mut self.arena,
                text_x,
                text_y,
                bytes.as_ptr() as *const _,
                bytes.len(),
                &style,
            );
            heartbeat();

            flux_canvas_end(self.canvas);
            heartbeat();
            let r = flux_frame_submit(frame);
            heartbeat();
            if !flux_result_is_ok(r) {
                return;
            }
            before_present();
            flux_frame_present(frame);
            heartbeat();
        }
    }

    /// Ensure the surface is big enough for a single-row banner of `label`.
    /// Mirrors `ensure_candidate_size`'s two-path strategy (ADR-0013):
    /// grow-only with `wp_viewport` when available, exact-size resize
    /// otherwise. Banner rows are usually narrower than candidate rows, so
    /// after a candidate-panel showing the swapchain typically reuses the
    /// existing quantum without any `vkDeviceWaitIdle`.
    pub fn ensure_banner_size(&mut self, label: &str) {
        let style = flux_text_style {
            size_px: BANNER_FONT_SIZE,
            weight: 400.0,
            // Colour is irrelevant for `flux_text_measure`; provide one to
            // keep the struct fully initialised.
            color: unsafe { flux_color_rgba(0, 0, 0, 0) },
            family: FontFamily::FLUX_TEXT_FAMILY_DEFAULT,
        };

        let bytes = label.as_bytes();
        let metrics = unsafe {
            flux_sys::flux_text_measure(self.text, bytes.as_ptr() as *const _, bytes.len(), &style)
        };
        let desired_width = (BANNER_PADDING * 2.0 + metrics.width).max(10.0).ceil() as u32;
        let desired_height = (BANNER_ROW_HEIGHT).ceil() as u32;

        let phys_width = (desired_width as f32 * self.scale).ceil() as u32;
        let phys_height = (desired_height as f32 * self.scale).ceil() as u32;

        let content_w_logical = desired_width as i32;
        let content_h_logical = desired_height as i32;

        self.apply_grow_only_size(
            phys_width,
            phys_height,
            content_w_logical,
            content_h_logical,
        );
    }
}

impl Drop for FluxPanel {
    fn drop(&mut self) {
        unsafe {
            flux_arena_destroy(&mut self.arena);
            if !self.text.is_null() {
                flux_text_destroy(self.text);
            }
            if !self.canvas.is_null() {
                flux_canvas_destroy(self.canvas);
            }
            if !self.surface.is_null() {
                flux_surface_release(self.surface);
            }
            if !self.device.is_null() {
                flux_device_release(self.device);
            }
            // Free the cache path AFTER flux_device_release so the
            // save callback (fired inside release) can still read it.
            free_cache_path(self.pipeline_cache_path);
        }
    }
}

fn candidate_number_label(index: usize) -> String {
    match index {
        0..=8 => (index + 1).to_string(),
        9 => "0".to_string(),
        _ => (index + 1).to_string(),
    }
}

// ── Raw Wayland surface creation via wayland-sys ──────────────────────────

unsafe fn wl_surface_detach_and_commit(wl_surface: *mut c_void) {
    if wl_surface.is_null() {
        return;
    }

    let surface = wl_surface as *mut wl_proxy;
    let mut attach_args = [
        wl_argument { o: ptr::null() },
        wl_argument { i: 0 },
        wl_argument { i: 0 },
    ];
    unsafe {
        wl_proxy_marshal_array(surface, 1, attach_args.as_mut_ptr());
    }

    let mut commit_args: [wl_argument; 0] = [];
    unsafe {
        wl_proxy_marshal_array(surface, 6, commit_args.as_mut_ptr());
    }
}

// ── Vulkan Wayland surface creation ───────────────────────────────────────

unsafe fn create_wayland_vk_surface(
    instance: *mut c_void,
    wl_display: *mut c_void,
    wl_surface: *mut c_void,
) -> Result<*mut c_void, String> {
    #[repr(C)]
    struct VkWaylandSurfaceCreateInfoKHR {
        s_type: u32,
        p_next: *mut c_void,
        flags: u32,
        display: *mut c_void,
        surface: *mut c_void,
    }

    let create_info = VkWaylandSurfaceCreateInfoKHR {
        s_type: 1000006000,
        p_next: ptr::null_mut(),
        flags: 0,
        display: wl_display,
        surface: wl_surface,
    };

    let lib = unsafe { libc::dlopen(c"libvulkan.so.1".as_ptr(), libc::RTLD_NOW) };
    if lib.is_null() {
        return Err("cannot load libvulkan.so.1".into());
    }

    let func: unsafe extern "C" fn(
        *mut c_void,
        *const VkWaylandSurfaceCreateInfoKHR,
        *const c_void,
        *mut *mut c_void,
    ) -> i32 = unsafe {
        let sym = libc::dlsym(lib, c"vkCreateWaylandSurfaceKHR".as_ptr());
        if sym.is_null() {
            libc::dlclose(lib);
            return Err("cannot find vkCreateWaylandSurfaceKHR".into());
        }
        std::mem::transmute(sym)
    };

    let mut vk_surface: *mut c_void = ptr::null_mut();
    let result = func(instance, &create_info, ptr::null(), &mut vk_surface);
    libc::dlclose(lib);

    if result != 0 {
        return Err(format!("vkCreateWaylandSurfaceKHR failed: {result}"));
    }
    Ok(vk_surface)
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn flux_result_is_ok(_r: flux_result) -> bool {
    let v: i32 = unsafe { std::mem::transmute(_r) };
    v == 0
}

fn flux_last_error_string(function: &str) -> String {
    let mut info: flux_error_info = unsafe { std::mem::zeroed() };
    unsafe { flux_get_last_error(&mut info) };
    let msg = if info.message.is_null() {
        "(no message)".to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr(info.message) }
            .to_string_lossy()
            .into_owned()
    };
    format!("{function} failed: {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_number_label_matches_selection_keys() {
        let labels: Vec<_> = (0..10).map(candidate_number_label).collect();
        assert_eq!(labels, ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"]);
        assert_eq!(candidate_number_label(10), "11");
    }

    #[test]
    fn flux_last_error_string_is_readable() {
        let s = flux_last_error_string("test_function");
        assert!(s.contains("test_function"));
    }
}
