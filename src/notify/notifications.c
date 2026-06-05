/**
 * @file notifications.c
 * @brief Desktop notification transport via org.freedesktop.Notifications
 */

#include "notifications.h"

#include "typio/abi/log.h"

#ifdef HAVE_LIBSYSTEMD
#  include <systemd/sd-bus.h>
#endif

#include <stdint.h>
#include <string.h>
#include <time.h>
#include <stdio.h>
#include <stdlib.h>

#define TYPIO_NOTIFY_SERVICE "org.freedesktop.Notifications"
#define TYPIO_NOTIFY_PATH "/org/freedesktop/Notifications"
#define TYPIO_NOTIFY_INTERFACE "org.freedesktop.Notifications"
#define TYPIO_NOTIFY_RECENT_CAP 16

typedef struct TypioRecentNotification {
    char key[96];
    uint64_t last_sent_ms;
} TypioRecentNotification;

struct TypioNotifier {
#ifdef HAVE_LIBSYSTEMD
    sd_bus *bus;
#endif
    TypioRecentNotification recent[TYPIO_NOTIFY_RECENT_CAP];
    size_t next_recent_slot;
};

static uint64_t typio_notify_monotonic_ms(void) {
    struct timespec ts;

    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint64_t)ts.tv_sec * 1000U + (uint64_t)ts.tv_nsec / 1000000U;
}

static bool notifier_is_rate_limited(TypioNotifier *notifier,
                                     const char *key,
                                     uint64_t cooldown_ms) {
    uint64_t now_ms;

    if (!notifier || !key || !*key || cooldown_ms == 0) {
        return false;
    }

    now_ms = typio_notify_monotonic_ms();
    for (size_t i = 0; i < TYPIO_NOTIFY_RECENT_CAP; ++i) {
        TypioRecentNotification *entry = &notifier->recent[i];
        if (entry->key[0] == '\0' || strcmp(entry->key, key) != 0) {
            continue;
        }
        if (now_ms >= entry->last_sent_ms &&
            now_ms - entry->last_sent_ms < cooldown_ms) {
            return true;
        }
        entry->last_sent_ms = now_ms;
        return false;
    }

    {
        TypioRecentNotification *entry =
            &notifier->recent[notifier->next_recent_slot % TYPIO_NOTIFY_RECENT_CAP];
        snprintf(entry->key, sizeof(entry->key), "%s", key);
        entry->last_sent_ms = now_ms;
        notifier->next_recent_slot =
            (notifier->next_recent_slot + 1U) % TYPIO_NOTIFY_RECENT_CAP;
    }
    return false;
}

#ifdef HAVE_LIBSYSTEMD
/*
 * Append the {sv} hints array carrying the urgency byte. sd-bus's
 * append API takes type sigils and a pointer; an 'a{sv}' is built by
 * entering the array, then the dict-entry, then the variant, then
 * appending the byte, then closing them in reverse order. On any
 * mid-construction failure we unwind the open containers and return
 * a negative error.
 */
static int append_hints(sd_bus_message *m, TypioNotificationUrgency urgency) {
    int r;
    unsigned char urgency_value = (unsigned char)urgency;

    r = sd_bus_message_open_container(m, 'a', "{sv}");
    if (r < 0) return r;

    r = sd_bus_message_open_container(m, 'e', "sv");
    if (r < 0) goto fail_array;

    r = sd_bus_message_append_basic(m, 's', "urgency");
    if (r < 0) goto fail_entry;

    r = sd_bus_message_open_container(m, 'v', "y");
    if (r < 0) goto fail_entry;

    r = sd_bus_message_append_basic(m, 'y', &urgency_value);
    if (r < 0) goto fail_variant;

    r = sd_bus_message_close_container(m);
    if (r < 0) goto fail_variant; /* close the 'v' */

    r = sd_bus_message_close_container(m);
    if (r < 0) return r;          /* close the 'e' */

    r = sd_bus_message_close_container(m);
    return r;                     /* close the 'a' */

fail_variant:
    sd_bus_message_close_container(m); /* close 'v' (best-effort) */
fail_entry:
    sd_bus_message_close_container(m); /* close 'e' (best-effort) */
fail_array:
    sd_bus_message_close_container(m); /* close 'a' (best-effort) */
    return r;
}
#endif

TypioNotifier *typio_notifier_new(void) {
    TypioNotifier *notifier;
#ifdef HAVE_LIBSYSTEMD
    int r;
#endif

    notifier = calloc(1, sizeof(*notifier));
    if (!notifier) {
        return nullptr;
    }

#ifdef HAVE_LIBSYSTEMD
    r = sd_bus_open_user(&notifier->bus);
    if (r < 0) {
        typio_log_warning("Desktop notifications unavailable: %s", strerror(-r));
        free(notifier);
        return nullptr;
    }
#endif

    return notifier;
}

void typio_notifier_free(TypioNotifier *notifier) {
    if (!notifier) {
        return;
    }

#ifdef HAVE_LIBSYSTEMD
    if (notifier->bus) {
        sd_bus_unref(notifier->bus);
    }
#endif
    free(notifier);
}

bool typio_notifier_send(TypioNotifier *notifier,
                         TypioNotificationUrgency urgency,
                         const char *summary,
                         const char *body) {
#ifdef HAVE_LIBSYSTEMD
    sd_bus_message *msg = nullptr;
    sd_bus_message *reply = nullptr;
    sd_bus_error err = SD_BUS_ERROR_NULL;
    const char *app_name = "Typio";
    const char *app_icon = "typio-keyboard-symbolic";
    const char *notification_summary = summary ? summary : "Typio";
    const char *notification_body = body ? body : "";
    uint32_t replaces_id = 0;
    int32_t expire_timeout = urgency == TYPIO_NOTIFICATION_CRITICAL ? 0 : 12000;
    int r;

    if (!notifier || !notifier->bus || !summary || !*summary) {
        return false;
    }

    r = sd_bus_message_new_method_call(notifier->bus,
                                       &msg,
                                       TYPIO_NOTIFY_SERVICE,
                                       TYPIO_NOTIFY_PATH,
                                       TYPIO_NOTIFY_INTERFACE,
                                       "Notify");
    if (r < 0) {
        typio_log_debug("Notification send: failed to build message: %s", strerror(-r));
        return false;
    }

    r = sd_bus_message_append(msg,
                              "susss",
                              app_name,
                              replaces_id,
                              app_icon,
                              notification_summary,
                              notification_body);
    if (r < 0) goto fail;

    /* actions: as (empty array) */
    r = sd_bus_message_open_container(msg, 'a', "s");
    if (r < 0) goto fail;
    r = sd_bus_message_close_container(msg);
    if (r < 0) goto fail;

    /* hints: a{sv} */
    r = append_hints(msg, urgency);
    if (r < 0) goto fail;

    /* expire_timeout: i */
    r = sd_bus_message_append_basic(msg, 'i', &expire_timeout);
    if (r < 0) goto fail;

    r = sd_bus_call_method(notifier->bus, msg, 0, &err, &reply);
    if (r < 0) {
        typio_log_debug("Notification send failed: %s", err.message);
        sd_bus_error_free(&err);
        goto fail;
    }

    sd_bus_message_unref(msg);
    sd_bus_message_unref(reply);
    return true;

fail:
    sd_bus_message_unref(msg);
    return false;
#else
    (void)notifier;
    (void)urgency;
    (void)summary;
    (void)body;
    return false;
#endif
}

bool typio_notifier_send_coalesced(TypioNotifier *notifier,
                                   const char *key,
                                   uint64_t cooldown_ms,
                                   TypioNotificationUrgency urgency,
                                   const char *summary,
                                   const char *body) {
    if (notifier_is_rate_limited(notifier, key, cooldown_ms)) {
        return true;
    }
    return typio_notifier_send(notifier, urgency, summary, body);
}
