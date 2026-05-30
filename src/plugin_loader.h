#ifndef TYPIOD_PLUGIN_LOADER_H
#define TYPIOD_PLUGIN_LOADER_H

#include "typio/runtime/registry.h"
#include "typio/runtime/instance.h"

/**
 * @brief Plugin discovery callback for TypioInstanceConfig.plugin_loader.
 *
 * Enumerates libtypio-engine-*.so files in @p dir, dlopen()s each,
 * resolves the engine entry points, and registers them with the
 * registry via typio_registry_register_plugin_keyboard/_voice. Core
 * calls this once per configured engine directory.
 *
 * @return Number of engines successfully registered.
 */
int typio_plugin_load_dir(TypioRegistry *registry,
                           const char *dir,
                           void *user_data);

/**
 * @brief Resolve the ordered list of engine directories to scan.
 *
 * Precedence (earlier shadows later; duplicates are skipped by the
 * registry): explicit @p cli_override, then $TYPIO_ENGINE_DIR, then the
 * per-user XDG data engines dir, then the compile-time system dir.
 *
 * Returns a NULL-terminated, heap-allocated array of heap-allocated
 * strings suitable for TypioInstanceConfig.engine_dirs. Free with
 * typio_engine_dirs_free.
 */
const char *const *typio_engine_dirs_build(const char *cli_override);
void typio_engine_dirs_free(const char *const *dirs);

/**
 * @brief Return the first discovered engine icon theme path.
 *
 * During plugin loading the host scans each engine directory for an
 * `icons/` subdirectory.  If found, its path is stored and returned here.
 * The caller must not free the returned pointer; it is owned by the
 * plugin loader and lives until process exit.
 *
 * @return Absolute path to an icon theme directory, or nullptr if none
 *         was discovered.
 */
const char *typio_plugin_discovered_icon_theme_path(void);

#endif /* TYPIOD_PLUGIN_LOADER_H */
