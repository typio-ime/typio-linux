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

use flux_sys::{
    flux_arena, flux_arena_destroy, flux_arena_init, flux_arena_reset, flux_canvas,
    flux_canvas_begin, flux_canvas_desc, flux_canvas_destroy, flux_canvas_end,
    flux_canvas_fill_rrect, flux_color_rgba, flux_device, flux_device_create, flux_device_desc,
    flux_device_release, flux_device_vk_instance, flux_error_info, flux_frame,
    flux_frame_begin_desc, flux_frame_present, flux_frame_submit, flux_get_last_error, flux_result,
    flux_struct_type, flux_surface, flux_surface_begin_frame, flux_surface_create,
    flux_surface_desc, flux_surface_release, flux_text, flux_text_create, flux_text_desc,
    flux_text_destroy, flux_text_draw, flux_text_family, flux_text_style,
};
use wayland_sys::{
    client::{wl_proxy, wl_proxy_marshal_array},
    common::wl_argument,
};

use flux_struct_type as FType;
use flux_text_family as FontFamily;

/// Frame-acquire timeout for panel rendering (200 ms). Short enough that
/// a stuck `vkAcquireNextImageKHR` (e.g. popup surface not yet configured
/// by the compositor) fails fast instead of blocking the main loop past
/// the watchdog's 3-second stuck threshold.
const PANEL_FRAME_TIMEOUT_NS: u64 = 200_000_000;
const PANEL_PADDING: f32 = 8.0;
const PANEL_ROW_HEIGHT: f32 = 24.0;
const CANDIDATE_FONT_SIZE: f32 = 16.0;
const CANDIDATE_ITEM_X_PADDING: f32 = 5.0;
const CANDIDATE_ITEM_GAP: f32 = 8.0;
const CANDIDATE_NUMBER_FONT_SIZE: f32 = 11.0;
const CANDIDATE_NUMBER_GAP: f32 = 4.0;

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
    // Keep the raw wl_surface alive for the panel's lifetime.
    _wl_surface: *mut c_void,
}

impl FluxPanel {
    /// Create a panel backed by a Wayland surface via Vulkan.
    ///
    /// `wl_display_ptr` must be a valid `*mut wl_display` obtained
    /// from the same Wayland connection the input-method frontend
    /// uses (via `Connection::backend().display_ptr()`).
    ///
    /// # Safety
    /// `wl_display_ptr` must be valid for the panel's lifetime.
    /// Create a panel backed by an EXISTING wl_surface (e.g. one that's
    /// already connected to zwp_input_popup_surface_v2 for positioning).
    /// The surface must outlive the panel.
    pub unsafe fn new_from_surface(
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        Self::new_inner(wl_display_ptr, wl_surface_ptr, width, height)
    }

    fn new_inner(
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        unsafe { Self::new_inner_unsafe(wl_display_ptr, wl_surface_ptr, width, height) }
    }

    unsafe fn new_inner_unsafe(
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
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
        let mut device_desc: flux_device_desc = std::mem::zeroed();
        device_desc.type_ = FType::FLUX_TYPE_DEVICE_DESC;
        device_desc.required_instance_extensions = instance_exts.as_ptr();
        device_desc.required_instance_extension_count = instance_exts.len() as u32;
        device_desc.required_device_extensions = device_exts.as_ptr();
        device_desc.required_device_extension_count = device_exts.len() as u32;
        device_desc.frames_in_flight = 2;

        let mut device: *mut flux_device = ptr::null_mut();
        let r = flux_device_create(&device_desc, &mut device);
        if !flux_result_is_ok(r) {
            return Err(flux_last_error_string("flux_device_create"));
        }

        // 3. Create VkSurfaceKHR from the wl_surface.
        let vk_instance = flux_device_vk_instance(device) as *mut c_void;
        let vk_surface = create_wayland_vk_surface(vk_instance, wl_display_ptr, wl_surface)?;
        if vk_surface.is_null() {
            flux_device_release(device);
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
            return Err(flux_last_error_string("flux_text_create"));
        }

        // 7. Create arena for per-frame text shaping allocations.
        let mut arena: flux_arena = unsafe { std::mem::zeroed() };
        let r = flux_arena_init(&mut arena, 256 * 1024, ptr::null_mut());
        if !flux_result_is_ok(r) {
            flux_text_destroy(text);
            flux_canvas_destroy(canvas);
            flux_surface_release(surface);
            flux_device_release(device);
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
            _wl_surface: wl_surface,
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
    pub fn draw_candidates(&mut self, candidates: &[String], selected: usize) {
        unsafe {
            flux_arena_reset(&mut self.arena);

            let frame_desc = flux_frame_begin_desc {
                type_: FType::FLUX_TYPE_FRAME_BEGIN_DESC,
                next: ptr::null(),
                timeout_ns: PANEL_FRAME_TIMEOUT_NS,
            };
            let mut frame: *mut flux_frame = ptr::null_mut();
            let r = flux_surface_begin_frame(self.surface, &frame_desc, &mut frame);
            if !flux_result_is_ok(r) {
                return;
            }

            let bg = flux_color_rgba(28, 28, 32, 255);
            let r = flux_canvas_begin(self.canvas, frame, &bg);
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
                let number_metrics = flux_sys::flux_text_measure(
                    self.text,
                    number_bytes.as_ptr() as *const _,
                    number_bytes.len(),
                    &number_style,
                );
                let bytes = candidate.as_bytes();
                let metrics = flux_sys::flux_text_measure(
                    self.text,
                    bytes.as_ptr() as *const _,
                    bytes.len(),
                    &style,
                );

                let item_width = CANDIDATE_ITEM_X_PADDING * 2.0
                    + number_metrics.width
                    + CANDIDATE_NUMBER_GAP
                    + metrics.width;
                let text_top = y + (PANEL_ROW_HEIGHT - metrics.height).max(0.0) / 2.0;
                let number_top = text_top + metrics.baseline - number_metrics.baseline;

                if i == selected {
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
                    );
                }

                flux_text_draw(
                    self.text,
                    self.canvas,
                    &mut self.arena,
                    current_x + CANDIDATE_ITEM_X_PADDING,
                    number_top,
                    number_bytes.as_ptr() as *const _,
                    number_bytes.len(),
                    &number_style,
                );
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
                );

                current_x += item_width + CANDIDATE_ITEM_GAP;
            }

            flux_canvas_end(self.canvas);
            let r = flux_frame_submit(frame);
            if !flux_result_is_ok(r) {
                return;
            }
            flux_frame_present(frame);
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

    /// Ensure the surface is big enough for `candidate_count` rows.
    /// Only calls `resize` when the computed dimensions differ from the
    /// current ones, avoiding unnecessary swapchain recreations.
    pub fn ensure_candidate_size(&mut self, candidates: &[String]) {
        let mut total_width: f32 = PANEL_PADDING;

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
        self.resize(phys_width, phys_height);
    }

    /// Hide the panel by detaching the current Wayland buffer.
    pub fn hide(&mut self) {
        unsafe {
            wl_surface_detach_and_commit(self._wl_surface);
        }
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
