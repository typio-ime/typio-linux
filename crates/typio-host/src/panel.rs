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
    flux_arena, flux_arena_destroy, flux_arena_init, flux_arena_reset,
    flux_canvas, flux_canvas_begin, flux_canvas_destroy, flux_canvas_desc,
    flux_canvas_end, flux_canvas_fill_rrect,
    flux_color_rgba,
    flux_device, flux_device_create, flux_device_desc, flux_device_release,
    flux_device_vk_instance, flux_frame, flux_frame_begin_desc,
    flux_frame_present, flux_get_last_error, flux_error_info,
    flux_result, flux_struct_type, flux_surface, flux_surface_begin_frame,
    flux_surface_create, flux_surface_desc, flux_surface_release,
    flux_text, flux_text_create, flux_text_desc, flux_text_destroy,
    flux_text_draw, flux_text_family, flux_text_style,
};

use flux_struct_type as FType;
use flux_text_family as FontFamily;

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
    pub unsafe fn new(
        wl_display_ptr: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        // 1. Create a wl_surface via raw libwayland-client.
        let wl_surface = raw_create_wl_surface(wl_display_ptr)?;
        if wl_surface.is_null() {
            return Err("cannot create wl_surface".into());
        }

        // 2. Create Vulkan device with Wayland surface extension.
        let wayland_ext = c"VK_KHR_wayland_surface".as_ptr() as *const c_char;
        let mut device_desc: flux_device_desc = std::mem::zeroed();
        device_desc.type_ = FType::FLUX_TYPE_DEVICE_DESC;
        device_desc.required_instance_extensions = &wayland_ext;
        device_desc.required_instance_extension_count = 1;

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
            _wl_surface: wl_surface,
        })
    }

    /// Draw candidate strings with the selected one highlighted.
    pub fn draw_candidates(&mut self, candidates: &[String], selected: usize) {
        unsafe {
            flux_arena_reset(&mut self.arena);

            let frame_desc: flux_frame_begin_desc = std::mem::zeroed();
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

            let row_height: f32 = 24.0;
            let padding: f32 = 8.0;
            let font_size: f32 = 16.0;
            let text_color = flux_color_rgba(240, 240, 240, 255);
            let highlight = flux_color_rgba(56, 84, 160, 255);

            let style = flux_text_style {
                size_px: font_size,
                weight: 400.0,
                color: text_color,
                family: FontFamily::FLUX_TEXT_FAMILY_DEFAULT,
            };

            for (i, candidate) in candidates.iter().enumerate() {
                let y = padding + i as f32 * row_height;

                if i == selected {
                    flux_canvas_fill_rrect(
                        self.canvas,
                        flux_sys::flux_rect {
                            x: 2.0,
                            y,
                            w: self.width as f32 - 4.0,
                            h: row_height,
                        },
                        4.0,
                        highlight,
                    );
                }

                let bytes = candidate.as_bytes();
                flux_text_draw(
                    self.text,
                    self.canvas,
                    &mut self.arena,
                    padding,
                    y + font_size,
                    bytes.as_ptr() as *const _,
                    bytes.len(),
                    &style,
                );
            }

            flux_canvas_end(self.canvas);
            flux_frame_present(frame);
        }
    }

    /// Resize the panel surface.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        if !self.surface.is_null() {
            unsafe {
                flux_sys::flux_surface_resize(self.surface, width, height);
            }
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

// ── Raw Wayland surface creation via wayland-sys ──────────────────────────

/// Callback context for registry global events.
struct RegistryCtx {
    compositor_name: u32,
    compositor_version: u32,
}

extern "C" fn registry_global(
    data: *mut c_void,
    _registry: *mut c_void,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    let ctx = unsafe { &mut *(data as *mut RegistryCtx) };
    let iface = unsafe { std::ffi::CStr::from_ptr(interface) };
    if iface.to_str().unwrap_or("") == "wl_compositor" {
        ctx.compositor_name = name;
        ctx.compositor_version = version.min(4);
    }
}

/// Create a wl_surface via raw libwayland-client C API.
///
/// Uses direct extern "C" FFI to libwayland-client.so (already loaded
/// by wayland-client). The wl_display pointer must be from the same
/// connection the input-method frontend uses.
unsafe fn raw_create_wl_surface(
    wl_display: *mut c_void,
) -> Result<*mut c_void, String> {
    let display = wl_display;

    // Get the registry.
    let registry = wl_display_get_registry(display);
    if registry.is_null() {
        return Err("wl_display_get_registry failed".into());
    }

    // Set up the registry listener to find wl_compositor.
    let mut ctx = RegistryCtx {
        compositor_name: 0,
        compositor_version: 0,
    };

    let listener = WlRegistryListener {
        global: Some(registry_global),
        global_remove: None,
    };

    wl_registry_add_listener(
        registry,
        &listener,
        &mut ctx as *mut RegistryCtx as *mut c_void,
    );

    // Roundtrip to receive the global events.
    wl_display_roundtrip(display);

    if ctx.compositor_name == 0 {
        return Err("compositor not found in registry".into());
    }

    // Find the wl_compositor_interface symbol (C global).
    let iface_ptr = libc::dlsym(
        libc::RTLD_DEFAULT,
        c"wl_compositor_interface".as_ptr(),
    );
    if iface_ptr.is_null() {
        return Err("cannot find wl_compositor_interface symbol".into());
    }

    // Bind the compositor.
    let compositor = wl_registry_bind(
        registry,
        ctx.compositor_name,
        iface_ptr,
        ctx.compositor_version,
    );

    if compositor.is_null() {
        return Err("wl_registry_bind for compositor failed".into());
    }

    // Create the surface.
    let surface = wl_compositor_create_surface(compositor);
    if surface.is_null() {
        return Err("wl_compositor_create_surface failed".into());
    }

    Ok(surface)
}

// Direct FFI to libwayland-client.so. These are stable C API functions
// that don't change between versions.
#[repr(C)]
struct WlRegistryListener {
    global: Option<extern "C" fn(*mut c_void, *mut c_void, u32, *const c_char, u32)>,
    global_remove: Option<extern "C" fn(*mut c_void, *mut c_void, u32)>,
}

extern "C" {
    fn wl_display_get_registry(display: *mut c_void) -> *mut c_void;
    fn wl_display_roundtrip(display: *mut c_void) -> i32;
    fn wl_registry_add_listener(
        registry: *mut c_void,
        listener: *const WlRegistryListener,
        data: *mut c_void,
    ) -> i32;
    fn wl_registry_bind(
        registry: *mut c_void,
        name: u32,
        interface: *const c_void,
        version: u32,
    ) -> *mut c_void;
    fn wl_compositor_create_surface(compositor: *mut c_void) -> *mut c_void;
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
    fn flux_last_error_string_is_readable() {
        let s = flux_last_error_string("test_function");
        assert!(s.contains("test_function"));
    }
}
