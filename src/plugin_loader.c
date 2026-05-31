#include "plugin_loader.h"

#include "typio/abi/log.h"
#include "typio/abi/engine.h"
#include "typio/abi/types.h"
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

static bool typio_register_one(TypioRegistry *registry, const char *path) {
    void *handle = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!handle) {
        typio_log_error("Failed to dlopen engine: %s (%s)", path, dlerror());
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

    if (!typio_negotiate_capabilities(path, info)) {
        dlclose(handle);
        return false;
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

static char *typio_user_engine_dir(void) {
    const char *home = getenv("HOME");
    if (!home || !home[0]) {
        return nullptr;
    }
    const char *suffix = "/.local/lib/typio/engines";
    size_t len = strlen(home) + strlen(suffix) + 1;
    char *result = malloc(len);
    if (result) {
        snprintf(result, len, "%s%s", home, suffix);
    }
    return result;
}

const char *const *typio_engine_dirs_build(const char *cli_override) {
    /* At most 4 entries + NULL terminator. */
    char **dirs = calloc(5, sizeof(char *));
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

    char *user_dir = typio_user_engine_dir();
    if (user_dir) {
        dirs[n++] = user_dir;
    }

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
