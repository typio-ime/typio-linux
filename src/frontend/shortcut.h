/**
 * @file wl_shortcut.h
 * @brief Cached shortcut bindings used by the Wayland frontend
 *
 * libtypio addresses shortcuts by string action-id; this struct caches
 * the resolved `TypioShortcutBinding` for each action the Wayland frontend
 * needs on the hot path, populated once at startup and on config reload
 * via @ref typio_wl_shortcut_config_load.
 */

#ifndef TYPIO_WL_SHORTCUT_H
#define TYPIO_WL_SHORTCUT_H

#include "typio/abi/shortcut.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypioConfig TypioConfig;

typedef struct TypioShortcutConfig {
    TypioShortcutBinding switch_engine;
    TypioShortcutBinding emergency_exit;
    TypioShortcutBinding voice_ptt;
} TypioShortcutConfig;

/** Resolve every cached binding from the live config (or built-in defaults
 *  if the config lookup misses). Safe to call repeatedly. */
void typio_shortcut_config_load(TypioShortcutConfig *out,
                                const TypioConfig *config);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_SHORTCUT_H */
