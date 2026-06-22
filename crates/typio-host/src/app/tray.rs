//! System-tray helpers.
//!
//! Free functions that translate between the live libtypio registry and
//! the Rust-side [`Tray`] / [`RegistrySnapshot`] surfaces. Split out of
//! `app/mod.rs` so the daemon's main loop file doesn't carry the registry
//! lookup boilerplate.

use std::ffi::CString;

use typio::c_api::registry as c_registry;
use typio::instance::TypioInstance;
use typio::TypioResult;

use crate::ipc_bus::TypioRegistryView;
use crate::service::SvcError;
use crate::state_controller::StateController;
use crate::tray_menu::{EngineDesc, RegistrySnapshot};
use crate::tray_sni::{MenuAction, Tray, TrayAction};

use super::DaemonEvent;

/// Wire the tray's action-handler callback to libtypio registry mutators.
/// Each menu action maps to either an immediate registry mutation
/// (engine/language/voice switch) or a daemon-level event (Restart /
/// Shutdown). Mutations emit `StateRefresh` so the loop re-syncs every
/// surface that mirrors the controller state.
#[cfg(feature = "systray")]
pub(super) fn install_tray_action_handler(
    tray: &Tray,
    instance: *mut TypioInstance,
    event_tx: std::sync::mpsc::Sender<DaemonEvent>,
) {
    // Cast to usize so the closure is Send; reconstruct inside each arm.
    let instance_ptr = instance as usize;
    tray.set_action_handler(move |action| {
        let instance = instance_ptr as *mut TypioInstance;
        let event = match action {
            TrayAction::Menu(MenuAction::Restart) => Some(DaemonEvent::Restart),
            TrayAction::Menu(MenuAction::Quit) => Some(DaemonEvent::Shutdown),
            TrayAction::Menu(MenuAction::Language(idx)) => {
                language_at_index(instance, idx as usize)
                    .and_then(|tag| set_active_language(instance, &tag).ok())
                    .map(|_| DaemonEvent::StateRefresh)
            }
            TrayAction::Menu(MenuAction::EngineInLanguage {
                lang_idx: _,
                engine_idx,
            }) => keyboard_at_index(instance, engine_idx as usize)
                .and_then(|name| set_active_keyboard(instance, &name).ok())
                .map(|_| DaemonEvent::StateRefresh),
            TrayAction::Menu(MenuAction::OrphanEngine(idx)) => {
                orphan_keyboard_at_index(instance, idx as usize)
                    .and_then(|name| set_active_keyboard(instance, &name).ok())
                    .map(|_| DaemonEvent::StateRefresh)
            }
            TrayAction::Menu(MenuAction::Voice(idx)) => voice_at_index(instance, idx as usize)
                .and_then(|name| set_active_voice(instance, &name).ok())
                .map(|_| DaemonEvent::StateRefresh),
            _ => None,
        };
        if let Some(event) = event {
            let _ = event_tx.send(event);
        }
    });
}

/// Push the current controller state (active engine name, status icon /
/// badge, full menu snapshot) into the tray.
#[cfg(feature = "systray")]
pub(super) fn update_tray_from_controller(
    tray: &Tray,
    controller: &StateController<TypioRegistryView>,
    instance: *mut TypioInstance,
) {
    tray.update_engine(controller.active_engine_name(), controller.engine_active());
    if controller.status_icon_is_badge() {
        tray.set_badge(controller.status_badge_text());
    } else {
        tray.set_icon(Some(controller.status_icon()));
    }
    if let Some(snapshot) = build_tray_snapshot(instance) {
        tray.set_menu_snapshot(snapshot);
    }
}

/// Build the menu snapshot from the live registry: known languages,
/// per-language keyboards, voice engines, and the active selections.
pub(super) fn build_tray_snapshot(instance: *mut TypioInstance) -> Option<RegistrySnapshot> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    let languages = reg.known_languages();
    let mut keyboards = Vec::new();
    for name in reg.list_keyboards() {
        let info = reg.engine_info(name)?;
        keyboards.push(EngineDesc {
            name: name.to_string(),
            display_name: Some(info.display_name.clone()),
            languages: info
                .effective_languages()
                .iter()
                .map(|s| s.to_string())
                .collect(),
        });
    }
    let mut voices = Vec::new();
    for name in reg.list_voices() {
        let info = reg.engine_info(name)?;
        voices.push(EngineDesc {
            name: name.to_string(),
            display_name: Some(info.display_name.clone()),
            languages: Vec::new(),
        });
    }
    Some(RegistrySnapshot {
        languages,
        active_language: reg.active_language().map(str::to_string),
        keyboards,
        voices,
        active_voice: reg.active_voice_name().map(str::to_string),
    })
}

pub(super) fn language_at_index(instance: *mut TypioInstance, idx: usize) -> Option<String> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    reg.known_languages().get(idx).cloned()
}

pub(super) fn keyboard_at_index(instance: *mut TypioInstance, idx: usize) -> Option<String> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    reg.list_keyboards().get(idx).map(|n| n.to_string())
}

pub(super) fn orphan_keyboard_at_index(instance: *mut TypioInstance, idx: usize) -> Option<String> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    let known: std::collections::HashSet<String> = reg.known_languages().into_iter().collect();
    let orphans: Vec<String> = reg
        .list_keyboards()
        .into_iter()
        .filter(|name| {
            reg.engine_info(name)
                .map(|info| info.effective_languages().iter().all(|l| !known.contains(l)))
                .unwrap_or(true)
        })
        .map(str::to_string)
        .collect();
    orphans.get(idx).cloned()
}

pub(super) fn voice_at_index(instance: *mut TypioInstance, idx: usize) -> Option<String> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    reg.list_voices().get(idx).map(|n| n.to_string())
}

pub(super) fn set_active_language(instance: *mut TypioInstance, tag: &str) -> Result<(), SvcError> {
    let reg = registry_ptr(instance).ok_or(SvcError)?;
    let tag_c = CString::new(tag).map_err(|_| SvcError)?;
    match c_registry::typio_registry_set_active_language(reg, tag_c.as_ptr()) {
        TypioResult::TypioOk => Ok(()),
        _ => Err(SvcError),
    }
}

pub(super) fn set_active_keyboard(
    instance: *mut TypioInstance,
    name: &str,
) -> Result<(), SvcError> {
    let reg = registry_ptr(instance).ok_or(SvcError)?;
    let name_c = CString::new(name).map_err(|_| SvcError)?;
    match c_registry::typio_registry_set_active_keyboard(reg, name_c.as_ptr()) {
        TypioResult::TypioOk => {
            eprintln!("tray: active keyboard -> {name}");
            Ok(())
        }
        _ => {
            eprintln!("tray: set_active_keyboard({name}) failed");
            Err(SvcError)
        }
    }
}

/// Cycle to the next registered keyboard engine, called when the user
/// presses the Ctrl+Shift engine-switch chord. Wraps from last back to
/// first; if only one keyboard is registered, the call is a no-op.
pub(super) fn cycle_active_keyboard(instance: *mut TypioInstance) {
    let Some(inst) = (unsafe { instance.as_ref() }) else {
        return;
    };
    let Some(reg) = inst.registry_rust() else {
        return;
    };
    let keyboards: Vec<&str> = reg.list_keyboards();
    if keyboards.len() < 2 {
        return;
    }
    let current: &str = reg.active_keyboard_name().unwrap_or(keyboards[0]);
    let next = keyboards
        .iter()
        .position(|k| *k == current)
        .and_then(|i| keyboards.get((i + 1) % keyboards.len()).copied())
        .unwrap_or(keyboards[0]);
    let _ = set_active_keyboard(instance, next);
}

pub(super) fn set_active_voice(instance: *mut TypioInstance, name: &str) -> Result<(), SvcError> {
    let reg = registry_ptr(instance).ok_or(SvcError)?;
    let name_c = CString::new(name).map_err(|_| SvcError)?;
    match c_registry::typio_registry_set_active_voice(reg, name_c.as_ptr()) {
        TypioResult::TypioOk => Ok(()),
        _ => Err(SvcError),
    }
}

pub(super) fn registry_ptr(
    instance: *mut TypioInstance,
) -> Option<*mut typio::c_api::registry::TypioRegistry> {
    if instance.is_null() {
        return None;
    }
    let reg = typio::instance::typio_instance_get_registry(instance);
    (reg as usize != 0).then_some(reg)
}
