#include "plugin_loader.h"

#include "typio/abi/log.h"
#include "typio/abi/engine.h"
#include "typio/abi/types.h"
#include "typio/abi/version.h"
#include "typio/schema/config_schema.h"
#include "typio_build_config.h"

#include <dirent.h>
#include <dlfcn.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define TYPIO_ENGINE_PREFIX "libtypio_engine_"
#define TYPIO_ENGINE_SUFFIX ".so"

static char *typio_discovered_icon_theme_path = NULL;

static void typio_plugin_close(void *handle) {
    if (handle) {
        dlclose(handle);
    }
}

static bool typio_is_engine_filename(const char *name) {
    size_t len = strlen(name);
    size_t pfx = strlen(TYPIO_ENGINE_PREFIX);
    size_t sfx = strlen(TYPIO_ENGINE_SUFFIX);
    if (len <= pfx + sfx) {
        return false;
    }
    if (strncmp(name, TYPIO_ENGINE_PREFIX, pfx) != 0) {
        return false;
    }
    return strcmp(name + len - sfx, TYPIO_ENGINE_SUFFIX) == 0;
}

/*
 * Capability negotiation.
 *
 * The host advertises a static set of capability names it can fulfil for
 * an engine.  An engine's `required_capabilities` array must be a subset
 * of this set or we refuse to load.  `optional_capabilities` produce an
 * info-level log when missing but never cause rejection.
 */
static const char *const TYPIOD_HOST_CAPABILITIES[] = {
    "preedit",
    "candidates",
    "prediction",
    "punctuation",
    "learning",
#ifdef HAVE_VOICE
    "voice_input",
    "continuous_voice",
#endif
    NULL,
};

static bool typio_host_supports(const char *capability) {
    for (size_t i = 0; TYPIOD_HOST_CAPABILITIES[i] != NULL; i++) {
        if (strcmp(TYPIOD_HOST_CAPABILITIES[i], capability) == 0) {
            return true;
        }
    }
    return false;
}

static bool typio_negotiate_capabilities(const char *path,
                                          const TypioEngineInfo *info) {
    if (info->required_capabilities) {
        for (size_t i = 0; info->required_capabilities[i] != NULL; i++) {
            const char *cap = info->required_capabilities[i];
            if (!typio_host_supports(cap)) {
                typio_log_error(
                    "Engine %s requires capability '%s' which the host does "
                    "not provide — refusing to load",
                    path, cap);
                return false;
            }
        }
    }
    if (info->optional_capabilities) {
        for (size_t i = 0; info->optional_capabilities[i] != NULL; i++) {
            const char *cap = info->optional_capabilities[i];
            if (!typio_host_supports(cap)) {
                typio_log_info(
                    "Engine %s optional capability '%s' is unavailable; "
                    "loading anyway",
                    path, cap);
            }
        }
    }
    return true;
}

static bool typio_is_engine_disabled(TypioRegistry *registry,
                                     const TypioEngineInfo *info) {
    TypioInstance *instance;
    TypioConfig *config;
    const char *disabled_str;
    const char *engine_name;
    const char *key;

    if (!registry || !info || !info->name) {
        return false;
    }

    instance = typio_registry_get_instance(registry);
    if (!instance) {
        return false;
    }

    config = typio_instance_get_config(instance);
    if (!config) {
        return false;
    }

    engine_name = info->name;

    if (info->type == TYPIO_ENGINE_TYPE_KEYBOARD) {
        key = "keyboard.disabled";
    } else if (info->type == TYPIO_ENGINE_TYPE_VOICE) {
        key = "voice.disabled";
    } else {
        return false;
    }

    disabled_str = typio_config_get_string(config, key, nullptr);
    if (!disabled_str || !*disabled_str) {
        return false;
    }

    size_t name_len = strlen(engine_name);
    const char *p = disabled_str;
    while (*p) {
        while (*p == ' ' || *p == '\t' || *p == ',') p++;
        if (!*p) break;
        const char *end = p;
        while (*end && *end != ',' && *end != ' ' && *end != '\t') end++;
        size_t tok_len = (size_t)(end - p);
        if (tok_len == name_len && strncmp(p, engine_name, name_len) == 0) {
            return true;
        }
        p = end;
    }

    return false;
}

static bool typio_register_one(TypioRegistry *registry, const char *path) {
    void *handle = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!handle) {
        typio_log_error("Failed to dlopen engine: %s (%s)", path, dlerror());
        return false;
    }

    /* ABI version check: must happen before any other plugin function calls */
    TypioEngineAbiVersionFunc abi_version_func =
        (TypioEngineAbiVersionFunc)dlsym(handle, "typio_engine_abi_version");
    if (!abi_version_func) {
        typio_log_error("Engine %s missing typio_engine_abi_version (built against old ABI?)", path);
        dlclose(handle);
        return false;
    }

    const TypioAbiVersion *plugin_abi = abi_version_func();
    if (!typio_engine_abi_check(plugin_abi)) {
        typio_log_error(
            "Engine %s ABI version mismatch: plugin=%u.%u, host=%u.%u — refusing to load",
            path,
            plugin_abi ? plugin_abi->major : 0,
            plugin_abi ? plugin_abi->minor : 0,
            TYPIO_ENGINE_ABI_MAJOR,
            TYPIO_ENGINE_ABI_MINOR);
        dlclose(handle);
        return false;
    }

    TypioEngineInfoFunc info_func =
        (TypioEngineInfoFunc)dlsym(handle, "typio_engine_get_info");
    if (!info_func) {
        typio_log_error("Engine %s missing typio_engine_get_info", path);
        dlclose(handle);
        return false;
    }

    const TypioEngineInfo *info = info_func();
    if (!info) {
        typio_log_error("Engine %s returned null info", path);
        dlclose(handle);
        return false;
    }

    if (typio_is_engine_disabled(registry, info)) {
        typio_log_info("Engine %s is disabled by configuration, skipping",
                       info->name ? info->name : "(null)");
        dlclose(handle);
        return false;
    }

    if (!typio_negotiate_capabilities(path, info)) {
        dlclose(handle);
        return false;
    }

    TypioEngineConfigSchemaFunc schema_func =
        (TypioEngineConfigSchemaFunc)dlsym(handle,
                                           "typio_engine_get_config_schema");
    if (schema_func) {
        size_t field_count = 0;
        const TypioConfigField *fields = schema_func(&field_count);
        if (fields && field_count > 0) {
            typio_config_schema_register_many(fields, field_count);
        }
    }

    TypioResult result;
    if (info->type == TYPIO_ENGINE_TYPE_VOICE) {
        TypioVoiceEngineFactory factory =
            (TypioVoiceEngineFactory)dlsym(handle, "typio_voice_engine_create");
        if (!factory) {
            typio_log_error("Engine %s missing typio_voice_engine_create", path);
            dlclose(handle);
            return false;
        }
        result = typio_registry_register_plugin_voice(
            registry, factory, info_func, handle, typio_plugin_close);
    } else {
        TypioKeyboardEngineFactory factory =
            (TypioKeyboardEngineFactory)dlsym(handle, "typio_keyboard_engine_create");
        if (!factory) {
            typio_log_error("Engine %s missing typio_keyboard_engine_create", path);
            dlclose(handle);
            return false;
        }
        result = typio_registry_register_plugin_keyboard(
            registry, factory, info_func, handle, typio_plugin_close);
    }

    if (result != TYPIO_OK) {
        /* register_plugin already closed the handle on failure. */
        typio_log_debug("Engine %s not registered (result %d)", path, result);
        return false;
    }
    return true;
}

int typio_plugin_load_dir(TypioRegistry *registry,
                           const char *dir,
                           void *user_data) {
    (void)user_data;
    if (!registry || !dir) {
        return 0;
    }

    DIR *d = opendir(dir);
    if (!d) {
        typio_log_debug("Cannot open engine directory: %s", dir);
        return 0;
    }

    int count = 0;
    struct dirent *ent;
    while ((ent = readdir(d)) != nullptr) {
        if (!typio_is_engine_filename(ent->d_name)) {
            continue;
        }
        char path[4096];
        int n = snprintf(path, sizeof(path), "%s/%s", dir, ent->d_name);
        if (n <= 0 || (size_t)n >= sizeof(path)) {
            continue;
        }
        if (typio_register_one(registry, path)) {
            count++;
        }
    }
    closedir(d);

    /* Discover bundled engine icons: <dir>/icons/ */
    if (!typio_discovered_icon_theme_path) {
        char icon_path[4096];
        int n = snprintf(icon_path, sizeof(icon_path), "%s/icons", dir);
        if (n > 0 && (size_t)n < sizeof(icon_path) && access(icon_path, R_OK) == 0) {
            typio_discovered_icon_theme_path = strdup(icon_path);
            typio_log_info("Discovered engine icon theme path: %s", icon_path);
        }
    }

    return count;
}

/* ── Engine directory resolution ──────────────────────────────────────── */

const char *const *typio_engine_dirs_build(const char *cli_override) {
    /* At most 3 entries + NULL terminator. */
    char **dirs = calloc(4, sizeof(char *));
    if (!dirs) {
        return nullptr;
    }
    size_t n = 0;

    if (cli_override && cli_override[0]) {
        dirs[n++] = strdup(cli_override);
    }

    const char *env_dir = getenv("TYPIO_ENGINE_DIR");
    if (env_dir && env_dir[0]) {
        dirs[n++] = strdup(env_dir);
    }

    /* Production defaults only load system-installed engines. Development and
     * test engines must be enabled explicitly via --engine-dir or
     * TYPIO_ENGINE_DIR. */
    if (TYPIO_ENGINE_DIR[0]) {
        dirs[n++] = strdup(TYPIO_ENGINE_DIR);
    }

    dirs[n] = nullptr;
    return (const char *const *)dirs;
}

void typio_engine_dirs_free(const char *const *dirs) {
    if (!dirs) {
        return;
    }
    for (size_t i = 0; dirs[i]; i++) {
        free((void *)dirs[i]);
    }
    free((void *)dirs);
}

const char *typio_plugin_discovered_icon_theme_path(void) {
    return typio_discovered_icon_theme_path;
}

/* ── Engine lifecycle: load/unload/reload ──────────────────────────────── */

bool typio_plugin_load_single(TypioRegistry *registry, const char *path) {
    if (!registry || !path) {
        return false;
    }

    /* Validate filename pattern */
    const char *basename = strrchr(path, '/');
    basename = basename ? basename + 1 : path;
    if (!typio_is_engine_filename(basename)) {
        typio_log_error("Plugin filename must match libtypio_engine_*.so: %s", path);
        return false;
    }

    /* Check if file exists */
    if (access(path, R_OK) != 0) {
        typio_log_error("Plugin file not readable: %s", path);
        return false;
    }

    typio_log_info("Loading engine from path: %s", path);
    return typio_register_one(registry, path);
}

bool typio_plugin_unload(TypioRegistry *registry, const char *name) {
    if (!registry || !name) {
        return false;
    }

    TypioResult result = typio_registry_unload(registry, name);
    if (result != TYPIO_OK) {
        if (result == TYPIO_ERROR_NOT_FOUND) {
            typio_log_warning("Engine not found: %s", name);
        } else {
            typio_log_error("Failed to unload engine %s: %d", name, result);
        }
        return false;
    }

    typio_log_info("Unloaded engine: %s", name);
    return true;
}

bool typio_plugin_reload(TypioRegistry *registry,
                          const char *name,
                          const char *path,
                          const char *const *engine_dirs) {
    if (!registry || !name) {
        return false;
    }

    /* Step 1: Unload the engine (ignore if not found) */
    typio_plugin_unload(registry, name);

    /* Step 2: Reload */
    if (path) {
        /* Explicit path provided */
        typio_log_info("Reloading engine %s from path: %s", name, path);
        return typio_plugin_load_single(registry, path);
    }

    /* Step 3: No path provided, scan engine_dirs to find by name */
    if (!engine_dirs) {
        typio_log_error("Cannot reload engine %s: no path and no engine_dirs", name);
        return false;
    }

    char target_filename[256];
    int n = snprintf(target_filename, sizeof(target_filename),
                     "%s%s%s", TYPIO_ENGINE_PREFIX, name, TYPIO_ENGINE_SUFFIX);
    if (n <= 0 || (size_t)n >= sizeof(target_filename)) {
        typio_log_error("Engine name too long: %s", name);
        return false;
    }

    for (size_t i = 0; engine_dirs[i]; i++) {
        const char *dir = engine_dirs[i];
        char full_path[4096];
        n = snprintf(full_path, sizeof(full_path), "%s/%s", dir, target_filename);
        if (n <= 0 || (size_t)n >= sizeof(full_path)) {
            continue;
        }

        if (access(full_path, R_OK) == 0) {
            typio_log_info("Reloading engine %s from directory: %s", name, dir);
            return typio_register_one(registry, full_path);
        }
    }

    typio_log_error("Engine %s not found in any configured directory", name);
    return false;
}
