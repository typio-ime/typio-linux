/**
 * @file health.c
 * @brief Startup health checks for desktop notifications
 */

#include "health.h"

#include "typio/abi/config.h"
#include "typio/abi/string.h"
#include "typio/runtime/registry.h"
#include "typio/runtime/instance.h"
#include "typio_build_config.h"

#include <stdio.h>
#include <string.h>

static bool startup_setting(const TypioConfig *config,
                            const char *key,
                            bool default_value) {
    if (!config) {
        return default_value;
    }
    return typio_config_get_bool(config, key, default_value);
}

static uint64_t startup_int_setting(const TypioConfig *config,
                                    const char *key,
                                    uint64_t default_value) {
    int value;

    if (!config) {
        return default_value;
    }

    value = typio_config_get_int(config, key, (int)default_value);
    if (value < 0) {
        return default_value;
    }
    return (uint64_t)value;
}

static void append_issue(TypioStartupIssue *issues,
                         size_t capacity,
                         size_t *count,
                         TypioStartupIssueSeverity severity,
                         const char *code,
                         const char *title,
                         const char *body) {
    TypioStartupIssue *issue;

    if (!count) {
        return;
    }

    if (*count >= capacity || !issues) {
        (*count)++;
        return;
    }

    issue = &issues[*count];
    memset(issue, 0, sizeof(*issue));
    issue->severity = severity;
    snprintf(issue->code, sizeof(issue->code), "%s", code ? code : "");
    snprintf(issue->title, sizeof(issue->title), "%s", title ? title : "");
    snprintf(issue->body, sizeof(issue->body), "%s", body ? body : "");
    (*count)++;
}

bool typio_startup_notifications_enabled(TypioInstance *instance) {
    TypioConfig *config = typio_instance_get_config(instance);
    return startup_setting(config, "notifications.enable", true);
}

bool typio_notifications_enabled(TypioInstance *instance) {
    return typio_startup_notifications_enabled(instance);
}

bool typio_startup_checks_enabled(TypioInstance *instance) {
    TypioConfig *config = typio_instance_get_config(instance);
    if (!startup_setting(config, "notifications.enable", true)) {
        return false;
    }
    return startup_setting(config, "notifications.startup_checks", true);
}

bool typio_runtime_notifications_enabled(TypioInstance *instance) {
    TypioConfig *config = typio_instance_get_config(instance);
    if (!startup_setting(config, "notifications.enable", true)) {
        return false;
    }
    return startup_setting(config, "notifications.runtime", true);
}

bool typio_voice_notifications_enabled(TypioInstance *instance) {
    TypioConfig *config = typio_instance_get_config(instance);
    if (!startup_setting(config, "notifications.enable", true)) {
        return false;
    }
    if (!startup_setting(config, "notifications.runtime", true)) {
        return false;
    }
    return startup_setting(config, "notifications.voice", true);
}

uint64_t typio_notification_cooldown_ms(TypioInstance *instance,
                                        uint64_t default_value) {
    TypioConfig *config = typio_instance_get_config(instance);
    return startup_int_setting(config, "notifications.cooldown_ms", default_value);
}

size_t typio_startup_health_collect(TypioInstance *instance,
                                    TypioStartupIssue *issues,
                                    size_t capacity) {
    TypioRegistry *registry;
    char *active_keyboard_name;
    size_t count = 0;

    if (!instance) {
        return 0;
    }

    registry = typio_instance_get_registry(instance);
    if (!registry) {
        append_issue(issues, capacity, &count, TYPIO_STARTUP_ISSUE_ERROR,
                     "engine-registry-missing",
                     "Typio startup incomplete",
                     "Engine registry is unavailable, so no input engine can be activated.");
        return count;
    }

    active_keyboard_name = typio_registry_get_active_keyboard(registry);

    if (!active_keyboard_name) {
        append_issue(issues, capacity, &count, TYPIO_STARTUP_ISSUE_ERROR,
                     "no-active-keyboard-engine",
                     "No keyboard engine is active",
                     "Typio started without an active keyboard engine. Check "
                     "your engine build/install state.");
    }

    typio_free_string(active_keyboard_name);
    return count;
}
