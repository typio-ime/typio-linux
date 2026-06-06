#include "engine_loader.h"

#include "typio/abi/log.h"
#include "typio/abi/config.h"
#include "typio/abi/engine.h"
#include "typio/abi/types.h"
#include "typio_build_config.h"

#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define TYPIO_ENGINE_MANIFEST_PREFIX "typio-engine-"
#define TYPIO_ENGINE_MANIFEST_SUFFIX ".toml"

static char *typio_discovered_icon_theme_path = NULL;

typedef struct {
    char *name;
    char *display_name;
    char *description;
    char *author;
    char *icon;
    char *language;
    char *type;
    char *command;
    char **args;
    size_t arg_count;
    char **required_caps;
    char **optional_caps;
} TypioEngineManifest;

static bool typio_is_manifest_filename(const char *name) {
    size_t len = strlen(name);
    size_t pfx = strlen(TYPIO_ENGINE_MANIFEST_PREFIX);
    size_t sfx = strlen(TYPIO_ENGINE_MANIFEST_SUFFIX);
    if (len <= pfx + sfx) {
        return false;
    }
    if (strncmp(name, TYPIO_ENGINE_MANIFEST_PREFIX, pfx) != 0) {
        return false;
    }
    return strcmp(name + len - sfx, TYPIO_ENGINE_MANIFEST_SUFFIX) == 0;
}

static char *trim_in_place(char *s) {
    while (*s == ' ' || *s == '\t' || *s == '\r' || *s == '\n') {
        s++;
    }
    char *end = s + strlen(s);
    while (end > s &&
           (end[-1] == ' ' || end[-1] == '\t' ||
            end[-1] == '\r' || end[-1] == '\n')) {
        *--end = '\0';
    }
    if (*s == '"' && end > s + 1 && end[-1] == '"') {
        s++;
        *--end = '\0';
    }
    return s;
}

static void string_array_free(char **items) {
    if (!items) {
        return;
    }
    for (size_t i = 0; items[i]; i++) {
        free(items[i]);
    }
    free(items);
}

static bool string_array_push(char ***items, size_t *count, const char *value) {
    char **next = realloc(*items, sizeof(char *) * (*count + 2));
    if (!next) {
        return false;
    }
    *items = next;
    (*items)[*count] = strdup(value);
    if (!(*items)[*count]) {
        return false;
    }
    (*count)++;
    (*items)[*count] = NULL;
    return true;
}

static char **split_list(const char *value) {
    if (!value || !*value) {
        return NULL;
    }
    char *copy = strdup(value);
    if (!copy) {
        return NULL;
    }
    char *body = trim_in_place(copy);
    size_t len = strlen(body);
    if (len >= 2 && body[0] == '[' && body[len - 1] == ']') {
        body[len - 1] = '\0';
        body++;
    }
    char **items = NULL;
    size_t count = 0;
    char *save = NULL;
    for (char *tok = strtok_r(body, ",", &save); tok;
         tok = strtok_r(NULL, ",", &save)) {
        tok = trim_in_place(tok);
        if (*tok && !string_array_push(&items, &count, tok)) {
            string_array_free(items);
            free(copy);
            return NULL;
        }
    }
    free(copy);
    return items;
}

static void manifest_free(TypioEngineManifest *m) {
    if (!m) {
        return;
    }
    free(m->name);
    free(m->display_name);
    free(m->description);
    free(m->author);
    free(m->icon);
    free(m->language);
    free(m->type);
    free(m->command);
    string_array_free(m->args);
    string_array_free(m->required_caps);
    string_array_free(m->optional_caps);
}

static bool manifest_set(char **field, const char *value) {
    char *copy = strdup(value);
    if (!copy) {
        return false;
    }
    free(*field);
    *field = copy;
    return true;
}

static char *resolve_manifest_path(const char *manifest_path, const char *value) {
    if (!value || !*value) {
        return NULL;
    }
    if (value[0] == '/') {
        return strdup(value);
    }
    const char *slash = strrchr(manifest_path, '/');
    if (!slash) {
        return strdup(value);
    }
    size_t dir_len = (size_t)(slash - manifest_path);
    size_t len = dir_len + 1 + strlen(value) + 1;
    char *path = malloc(len);
    if (!path) {
        return NULL;
    }
    memcpy(path, manifest_path, dir_len);
    path[dir_len] = '/';
    strcpy(path + dir_len + 1, value);
    return path;
}

static char *resolve_manifest_arg(const char *manifest_path, const char *value) {
    if (!value || !*value) {
        return NULL;
    }
    if (value[0] == '/' || strchr(value, '/')) {
        return resolve_manifest_path(manifest_path, value);
    }
    return strdup(value);
}

static bool manifest_parse(const char *path, TypioEngineManifest *m) {
    FILE *f = fopen(path, "r");
    if (!f) {
        typio_log_error("Cannot open engine manifest: %s", path);
        return false;
    }

    char line[4096];
    while (fgets(line, sizeof(line), f)) {
        char *p = trim_in_place(line);
        if (!*p || *p == '#') {
            continue;
        }
        char *eq = strchr(p, '=');
        if (!eq) {
            continue;
        }
        *eq = '\0';
        char *key = trim_in_place(p);
        char *value = trim_in_place(eq + 1);
        if (strcmp(key, "name") == 0) {
            if (!manifest_set(&m->name, value)) goto oom;
        } else if (strcmp(key, "display_name") == 0) {
            if (!manifest_set(&m->display_name, value)) goto oom;
        } else if (strcmp(key, "description") == 0) {
            if (!manifest_set(&m->description, value)) goto oom;
        } else if (strcmp(key, "author") == 0) {
            if (!manifest_set(&m->author, value)) goto oom;
        } else if (strcmp(key, "icon") == 0) {
            if (!manifest_set(&m->icon, value)) goto oom;
        } else if (strcmp(key, "language") == 0) {
            if (!manifest_set(&m->language, value)) goto oom;
        } else if (strcmp(key, "type") == 0) {
            if (!manifest_set(&m->type, value)) goto oom;
        } else if (strcmp(key, "command") == 0) {
            char *resolved = resolve_manifest_arg(path, value);
            if (!resolved) goto oom;
            free(m->command);
            m->command = resolved;
        } else if (strcmp(key, "arg") == 0) {
            char *resolved = resolve_manifest_arg(path, value);
            if (!resolved) goto oom;
            bool pushed = string_array_push(&m->args, &m->arg_count, resolved);
            free(resolved);
            if (!pushed) goto oom;
        } else if (strcmp(key, "args") == 0) {
            char **items = split_list(value);
            for (size_t i = 0; items && items[i]; i++) {
                char *resolved = resolve_manifest_arg(path, items[i]);
                if (!resolved) {
                    string_array_free(items);
                    goto oom;
                }
                bool pushed = string_array_push(&m->args, &m->arg_count, resolved);
                free(resolved);
                if (!pushed) {
                    string_array_free(items);
                    goto oom;
                }
            }
            string_array_free(items);
        } else if (strcmp(key, "required") == 0) {
            string_array_free(m->required_caps);
            m->required_caps = split_list(value);
        } else if (strcmp(key, "optional") == 0) {
            string_array_free(m->optional_caps);
            m->optional_caps = split_list(value);
        }
    }
    fclose(f);
    return true;

oom:
    fclose(f);
    typio_log_error("Out of memory while reading engine manifest: %s", path);
    return false;
}

/*
 * Capability negotiation.
 *
 * The host advertises a static set of capability names it can fulfil for an
 * engine. An engine's required capabilities must be a subset of this set or
 * the manifest is rejected.
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
                                          const char *const *required,
                                          const char *const *optional) {
    if (required) {
        for (size_t i = 0; required[i] != NULL; i++) {
            if (!typio_host_supports(required[i])) {
                typio_log_error(
                    "Engine manifest %s requires capability '%s' which the "
                    "host does not provide; refusing to load",
                    path, required[i]);
                return false;
            }
        }
    }
    if (optional) {
        for (size_t i = 0; optional[i] != NULL; i++) {
            if (!typio_host_supports(optional[i])) {
                typio_log_info(
                    "Engine manifest %s optional capability '%s' is unavailable",
                    path, optional[i]);
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

    disabled_str = typio_config_get_string(config, key, NULL);
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

static char **build_argv(const TypioEngineManifest *m) {
    if (!m->command) {
        return NULL;
    }
    char **argv = calloc(m->arg_count + 2, sizeof(char *));
    if (!argv) {
        return NULL;
    }
    argv[0] = strdup(m->command);
    if (!argv[0]) {
        free(argv);
        return NULL;
    }
    for (size_t i = 0; i < m->arg_count; i++) {
        argv[i + 1] = strdup(m->args[i]);
        if (!argv[i + 1]) {
            string_array_free(argv);
            return NULL;
        }
    }
    argv[m->arg_count + 1] = NULL;
    return argv;
}

static bool typio_register_one(TypioRegistry *registry, const char *path) {
    TypioEngineManifest m = {0};
    bool ok = false;

    if (!manifest_parse(path, &m)) {
        goto out;
    }
    if (!m.name || !m.type || !m.command) {
        typio_log_error("Engine manifest missing required fields: %s", path);
        goto out;
    }

    TypioEngineType type;
    if (strcmp(m.type, "keyboard") == 0) {
        type = TYPIO_ENGINE_TYPE_KEYBOARD;
    } else if (strcmp(m.type, "voice") == 0) {
        type = TYPIO_ENGINE_TYPE_VOICE;
    } else {
        typio_log_error("Engine manifest %s has invalid type '%s'", path, m.type);
        goto out;
    }

    TypioEngineInfo info = {
        .name = m.name,
        .display_name = m.display_name ? m.display_name : m.name,
        .description = m.description ? m.description : "",
        .author = m.author ? m.author : "",
        .icon = m.icon,
        .language = m.language ? m.language : "und",
        .type = type,
        .required_capabilities = (const char *const *)m.required_caps,
        .optional_capabilities = (const char *const *)m.optional_caps,
    };

    if (typio_is_engine_disabled(registry, &info)) {
        typio_log_info("Engine %s is disabled by configuration, skipping",
                       info.name);
        goto out;
    }
    if (!typio_negotiate_capabilities(path,
                                      info.required_capabilities,
                                      info.optional_capabilities)) {
        goto out;
    }

    char **argv = build_argv(&m);
    if (!argv) {
        typio_log_error("Engine manifest %s has no worker argv", path);
        goto out;
    }

    TypioResult result = typio_registry_register_ipc_engine(
        registry, &info, (const char *const *)argv);
    string_array_free(argv);
    if (result != TYPIO_OK) {
        typio_log_debug("Engine manifest %s not registered (result %d)",
                        path, result);
        goto out;
    }
    typio_log_info("Registered IPC engine %s from %s", info.name, path);
    ok = true;

out:
    manifest_free(&m);
    return ok;
}

int typio_engine_loader_load_dir(TypioRegistry *registry,
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
    while ((ent = readdir(d)) != NULL) {
        if (!typio_is_manifest_filename(ent->d_name)) {
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
        if (n > 0 && (size_t)n < sizeof(icon_path) &&
            access(icon_path, R_OK) == 0) {
            typio_discovered_icon_theme_path = strdup(icon_path);
            typio_log_info("Discovered engine icon theme path: %s", icon_path);
        }
    }

    return count;
}

/* ── Engine directory resolution ──────────────────────────────────────── */

const char *const *typio_engine_dirs_build(const char *const *cli_dirs,
                                           size_t cli_count) {
    const char *env_path = getenv("TYPIO_ENGINE_PATH");

    size_t env_max = 0;
    if (env_path && env_path[0]) {
        env_max = 1;
        for (const char *p = env_path; *p; p++) {
            if (*p == ':') {
                env_max++;
            }
        }
    }
    size_t cap = cli_count + env_max + 1;
    char **dirs = calloc(cap + 1, sizeof(char *));
    if (!dirs) {
        return NULL;
    }
    size_t n = 0;

    for (size_t i = 0; i < cli_count; i++) {
        if (cli_dirs[i] && cli_dirs[i][0]) {
            dirs[n++] = strdup(cli_dirs[i]);
        }
    }

    if (env_path && env_path[0]) {
        char *copy = strdup(env_path);
        if (copy) {
            char *save = NULL;
            for (char *tok = strtok_r(copy, ":", &save); tok != NULL;
                 tok = strtok_r(NULL, ":", &save)) {
                if (tok[0]) {
                    dirs[n++] = strdup(tok);
                }
            }
            free(copy);
        }
    }

    if (TYPIO_ENGINE_DIR[0]) {
        dirs[n++] = strdup(TYPIO_ENGINE_DIR);
    }

    dirs[n] = NULL;
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

const char *typio_engine_loader_discovered_icon_theme_path(void) {
    return typio_discovered_icon_theme_path;
}

bool typio_engine_loader_load_single(TypioRegistry *registry, const char *path) {
    if (!registry || !path) {
        return false;
    }
    const char *basename = strrchr(path, '/');
    basename = basename ? basename + 1 : path;
    if (!typio_is_manifest_filename(basename)) {
        typio_log_error("Engine manifest filename must match typio-engine-*.toml: %s",
                        path);
        return false;
    }
    if (access(path, R_OK) != 0) {
        typio_log_error("Engine manifest not readable: %s", path);
        return false;
    }
    return typio_register_one(registry, path);
}

bool typio_engine_loader_unload(TypioRegistry *registry, const char *name) {
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

bool typio_engine_loader_reload(TypioRegistry *registry,
                                const char *name,
                                const char *path,
                                const char *const *engine_dirs) {
    if (!registry || !name) {
        return false;
    }

    typio_engine_loader_unload(registry, name);

    if (path) {
        typio_log_info("Reloading engine %s from manifest: %s", name, path);
        return typio_engine_loader_load_single(registry, path);
    }

    if (!engine_dirs) {
        typio_log_error("Cannot reload engine %s: no path and no engine_dirs", name);
        return false;
    }

    char target_filename[256];
    int n = snprintf(target_filename, sizeof(target_filename),
                     "%s%s%s", TYPIO_ENGINE_MANIFEST_PREFIX, name,
                     TYPIO_ENGINE_MANIFEST_SUFFIX);
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
