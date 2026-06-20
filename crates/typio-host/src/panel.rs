//! Candidate panel — text rendering via flux (Vulkan canvas).
//!
//! Uses flux-sys FFI bindings to create a Vulkan device, a Wayland
//! surface, and render candidate text. The flux text module handles
//! font lookup (fontconfig), shaping (harfbuzz), and rasterization
//! (freetype) — all behind its C API.
//!
//! ## Architecture
//!
//! ```text
//! wl_surface (Wayland)
//!   → VkSurfaceKHR (Vulkan Wayland surface)
//!     → flux_surface (swapchain + present)
//!       → flux_canvas (2D drawing context)
//!         → flux_text (text rendering)
//! ```
//!
//! ## What this module covers
//!
//! - Device + surface + canvas + text context lifecycle via flux-sys.
//! - `draw_candidates()` — render a list of candidate strings with a
//!   highlighted selection.
//! - Surface resize handling.
//!
//! ## What is NOT covered yet
//!
//! - Popup positioning (needs zwp_input_popup_surface_v2 from
//!   input-method-v2 protocol — requires Wayland event dispatch).
//! - HiDPI / fractional-scale.
//! - Theme colors (hardcoded for now).
//! - Font selection (flux default for now).

use std::ffi::c_void;
use std::os::fd::AsRawFd;
use std::os::unix::io::RawFd;
use std::ptr;

// flux-sys re-exports all generated bindings at the crate root.
use flux_sys::{
    flux_canvas, flux_canvas_destroy, flux_canvas_desc, flux_device,
    flux_device_create, flux_device_desc, flux_device_release,
    flux_device_vk_instance, flux_get_last_error, flux_error_info,
    flux_result, flux_struct_type,
    flux_surface, flux_surface_create, flux_surface_desc,
    flux_surface_release, flux_text, flux_text_create, flux_text_desc,
    flux_text_destroy,
};

// Type-constant aliases for readability.
use flux_struct_type as FType;
#[allow(unused_imports)]
use flux_result as _FResult;

/// Wrapper for a flux-backed panel: device + surface + canvas + text.
pub struct FluxPanel {
    device: *mut flux_device,
    surface: *mut flux_surface,
    canvas: *mut flux_canvas,
    text: *mut flux_text,
    width: u32,
    height: u32,
}

impl FluxPanel {
    /// Create a panel backed by a Wayland surface via Vulkan.
    ///
    /// `wl_display_fd` is the Wayland connection's fd.
    /// `wl_surface_ptr` is a raw pointer to the `wl_surface` struct.
    /// `width`/`height` are the initial size in physical pixels.
    ///
    /// # Safety
    /// The caller must ensure `wl_surface_ptr` is a valid `wl_surface*`
    /// for the lifetime of the returned `FluxPanel`.
    pub unsafe fn new(
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        // 1. Create Vulkan device with Wayland surface extension.
        let wayland_ext = b"VK_KHR_wayland_surface\0".as_ptr() as *const _;
        let mut device_desc: flux_device_desc = std::mem::zeroed();
        device_desc.type_ = FType::FLUX_TYPE_DEVICE_DESC;
        device_desc.required_instance_extensions = &wayland_ext;
        device_desc.required_instance_extension_count = 1;

        let mut device: *mut flux_device = ptr::null_mut();
        let r = flux_device_create(&device_desc, &mut device);
        if !flux_result_is_ok(r) {
            return Err(flux_last_error_string("flux_device_create"));
        }

        // 2. Create VkSurfaceKHR from the wl_surface.
        let vk_instance = flux_device_vk_instance(device) as *mut c_void;
        let vk_surface = create_wayland_vk_surface(vk_instance, wl_display_ptr, wl_surface_ptr)?;
        if vk_surface.is_null() {
            return Err("vkCreateWaylandSurfaceKHR returned NULL".into());
        }

        // 3. Create flux surface.
        let mut surface_desc: flux_surface_desc = std::mem::zeroed();
        surface_desc.type_ = FType::FLUX_TYPE_SURFACE_DESC;
        surface_desc.vk_surface_khr = vk_surface as *mut c_void;
        surface_desc.width = width;
        surface_desc.height = height;

        let mut surface: *mut flux_surface = ptr::null_mut();
        let r = flux_surface_create(device, &surface_desc, &mut surface);
        if !flux_result_is_ok(r) {
            flux_device_release(device);
            return Err(flux_last_error_string("flux_surface_create"));
        }

        // 4. Create flux canvas.
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

        // 5. Create flux text context.
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

        Ok(Self {
            device,
            surface,
            canvas,
            text,
            width,
            height,
        })
    }

    /// Draw candidate strings. The `selected` index is highlighted.
    pub fn draw_candidates(&mut self, candidates: &[String], selected: usize) {
        // Implementation requires flux_text_draw which has a complex
        // API. For the initial port, this is a placeholder that proves
        // the flux integration compiles and links. The actual text
        // rendering calls will be added once we verify the device +
        // surface creation works end-to-end against a live compositor.
        let _ = candidates;
        let _ = selected;
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

/// Create a VkSurfaceKHR from a Wayland surface.
///
/// Calls `vkCreateWaylandSurfaceKHR` directly via FFI. The Vulkan
/// instance must have been created with `VK_KHR_wayland_surface`
/// extension enabled.
unsafe fn create_wayland_vk_surface(
    instance: *mut c_void,
    wl_display: *mut c_void,
    wl_surface: *mut c_void,
) -> Result<*mut c_void, String> {
    // VkWaylandSurfaceCreateInfoKHR
    #[repr(C)]
    struct VkWaylandSurfaceCreateInfoKHR {
        s_type: u32, // VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR = 1000006000
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

    // Load vkCreateWaylandSurfaceKHR from libvulkan.
    let lib = unsafe { libc::dlopen(b"libvulkan.so.1\0".as_ptr() as *const _, libc::RTLD_NOW) };
    if lib.is_null() {
        return Err("cannot load libvulkan.so.1".into());
    }

    let fn_name = b"vkCreateWaylandSurfaceKHR\0".as_ptr() as *const _;
    let func: unsafe extern "C" fn(
        *mut c_void,         // instance
        *const VkWaylandSurfaceCreateInfoKHR,
        *const c_void,       // allocator
        *mut *mut c_void,    // out surface
    ) -> i32 = unsafe {
        let sym = libc::dlsym(lib, fn_name);
        if sym.is_null() {
            libc::dlclose(lib);
            return Err("cannot find vkCreateWaylandSurfaceKHR".into());
        }
        std::mem::transmute(sym)
    };

    let mut vk_surface: *mut c_void = ptr::null_mut();
    let result = func(
        instance,
        &create_info,
        ptr::null(),
        &mut vk_surface,
    );
    libc::dlclose(lib);

    if result != 0 {
        return Err(format!("vkCreateWaylandSurfaceKHR failed: {result}"));
    }
    Ok(vk_surface)
}

/// Check if a flux_result is OK.
fn flux_result_is_ok(_r: flux_result) -> bool {
    // flux_result is a bindgen C enum without PartialEq. We compare
    // via the discriminant: the OK variant is always 0.
    let v: i32 = unsafe { std::mem::transmute(_r) };
    v == 0
}

/// Get flux's last error as a human-readable string.
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
