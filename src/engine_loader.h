#ifndef TYPIOD_ENGINE_LOADER_H
#define TYPIOD_ENGINE_LOADER_H

#include "typio/runtime/registry.h"
#include "typio/runtime/instance.h"

/**
 * @brief Engine manifest discovery callback for TypioInstanceConfig.plugin_loader.
 *
 * Enumerates `typio-engine-*.toml` manifests in @p dir, resolves each
 * manifest's worker argv, and registers out-of-process IPC engines via
 * `typio_registry_register_ipc_engine`. Core calls this once per configured
 * engine directory.
 *
 * @return Number of engines successfully registered.
 */
int typio_engine_loader_load_dir(TypioRegistry *registry,
                                 const char *dir,
                                 void *user_data);

/**
 * @brief Resolve the ordered list of engine directories to scan (ADR-0025).
 *
 * Precedence, highest first (the registry registers the first engine of each
 * name and skips later duplicates): the @p cli_count entries of @p cli_dirs in
 * order, then each colon-separated segment of $TYPIO_ENGINE_PATH in order,
 * then the compile-time system directory. There is no per-user auto-scan: the
 * daemon auto-loads only from the trusted system directory; every other source
 * is an explicit operator opt-in.
 *
 * Returns a NULL-terminated, heap-allocated array of heap-allocated
 * strings suitable for TypioInstanceConfig.engine_dirs. Free with
 * typio_engine_dirs_free.
 */
const char *const *typio_engine_dirs_build(const char *const *cli_dirs,
                                           size_t cli_count);
void typio_engine_dirs_free(const char *const *dirs);

/**
 * @brief Return the first discovered engine icon theme path.
 *
 * During engine loading the host scans each engine directory for an
 * `icons/` subdirectory.  If found, its path is stored and returned here.
 * The caller must not free the returned pointer; it is owned by the
 * engine loader and lives until process exit.
 *
 * @return Absolute path to an icon theme directory, or nullptr if none
 *         was discovered.
 */
const char *typio_engine_loader_discovered_icon_theme_path(void);

/**
 * @brief Load a single engine from a specific manifest path.
 *
 * @param registry  Engine registry.
 * @param path      Absolute path to a `typio-engine-*.toml` file.
 * @return true if loaded successfully, false otherwise.
 */
bool typio_engine_loader_load_single(TypioRegistry *registry, const char *path);

/**
 * @brief Unload an engine by name.
 *
 * Deactivates the engine if active, then destroys and unregisters it.
 *
 * @param registry  Engine registry.
 * @param name      Engine name (e.g. "rime").
 * @return true if unloaded, false if not found.
 */
bool typio_engine_loader_unload(TypioRegistry *registry, const char *name);

/**
 * @brief Reload an engine: unload then reload.
 *
 * If @p path is provided, reloads from that exact manifest. If @p path is
 * NULL, rescans all engine_dirs to find the engine by name.
 *
 * @param registry    Engine registry.
 * @param name        Engine name to reload.
 * @param path        Optional: explicit path to load from (may be NULL).
 * @param engine_dirs NULL-terminated array of directories to scan if path is NULL.
 * @return true if reloaded successfully, false otherwise.
 */
bool typio_engine_loader_reload(TypioRegistry *registry,
                                const char *name,
                                const char *path,
                                const char *const *engine_dirs);

#endif /* TYPIOD_ENGINE_LOADER_H */
