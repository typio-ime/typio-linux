/**
 * @file internal.h
 * @brief Internal structures for Wayland frontend
 */

#ifndef TYPIO_WL_FRONTEND_INTERNAL_H
#define TYPIO_WL_FRONTEND_INTERNAL_H

#include "frontend.h"
#include "input/wayland/arbiter.h"
#include "input/policy/tracker.h"
#include "typio/abi/shortcut.h"
#include "shortcut.h"
#include "engine/session_controller.h"
#include "input/wayland/repeat.h"
#include "engine/niri/identity.h"
#include "panel.h"
#include "engine/logind/resume.h"
#include "startup.h"
#include "input/wayland/bridge.h"
#include "panel_scheduler.h"
#include "typio/abi/input_context.h"
#include "typio/abi/types.h"
#include "typio_build_config.h"

#include <wayland-client.h>
#include <pthread.h>
#include <xkbcommon/xkbcommon.h>
#include <stdio.h>
#include <stdint.h>

#include "input-method-unstable-v2-client-protocol.h"
#include "virtual-keyboard-unstable-v1-client-protocol.h"
#include "fractional-scale-v1-client-protocol.h"
#include "viewporter-client-protocol.h"

#include "aux_handler.h"

#ifdef HAVE_VOICE
#include "typio/abi/voice.h"
#include "voice/pw_capture.h"
#endif

#ifdef __cplusplus
#include <atomic>
#define TYPIO_ATOMIC(t) std::atomic<t>
#else
#include <stdatomic.h>
#define TYPIO_ATOMIC(t) _Atomic t
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Forward declarations */
typedef struct TypioWlSession TypioWlSession;
typedef struct TypioWlKeyboard TypioWlKeyboard;
typedef struct TypioPanel TypioPanel;
typedef struct TypioWlOutput TypioWlOutput;
typedef struct TypioWlPanelCoordinator TypioWlPanelCoordinator;
typedef struct TypioWlVirtualKeyboard TypioWlVirtualKeyboard;
typedef struct TypioWlWatchdog TypioWlWatchdog;
typedef struct TypioWlKeyTracker TypioWlKeyTracker;
typedef struct TypioWlIndicator TypioWlIndicator;
typedef struct TypioWlConfigWatcher TypioWlConfigWatcher;

typedef enum TypioWlUiOwner {
    TYPIO_WL_UI_OWNER_NONE = 0,
    TYPIO_WL_UI_OWNER_CANDIDATE = 1,
    TYPIO_WL_UI_OWNER_INDICATOR = 2,
    TYPIO_WL_UI_OWNER_VOICE = 3,
} TypioWlUiOwner;

typedef enum TypioWlLoopStage {
    TYPIO_WL_LOOP_STAGE_IDLE = 0,
    TYPIO_WL_LOOP_STAGE_PANEL_UPDATE,
    TYPIO_WL_LOOP_STAGE_PREPARE_READ,
    TYPIO_WL_LOOP_STAGE_FLUSH,
    TYPIO_WL_LOOP_STAGE_POLL,
    TYPIO_WL_LOOP_STAGE_READ_EVENTS,
    TYPIO_WL_LOOP_STAGE_DISPATCH_PENDING,
    TYPIO_WL_LOOP_STAGE_AUX_IO,
    TYPIO_WL_LOOP_STAGE_REPEAT,
    TYPIO_WL_LOOP_STAGE_CONFIG_RELOAD,
} TypioWlLoopStage;

#define TYPIO_WL_MAX_TRACKED_KEYS 512

/**
 * @brief Per-key tracking state.
 *
 * This state machine records what happened to a key after routing decided
 * whether to consume or forward it. It is not itself the routing decision
 * model; action/reason routing decisions live in input/router.*.
 *
 * Each key can be in exactly one tracking state. Transitions:
 *
 *   IDLE ──press──▶ TRACK_FORWARDED          (forwarded to app)
 *   IDLE ──press──▶ TRACK_APP_SHORTCUT       (application shortcut bypasses engine)
 *   TRACK_APP_SHORTCUT ─physical release──▶ IDLE
 *   TRACK_FORWARDED ─force release──▶ TRACK_RELEASED_PENDING
 *   TRACK_FORWARDED ─physical release──▶ IDLE
 *   TRACK_RELEASED_PENDING ─physical release──▶ IDLE  (consumed)
 *   TRACK_SUPPRESSED_STARTUP ─physical release──▶ IDLE
 *   TRACK_ENGINE_NOT_READY ─physical release──▶ IDLE  (consumed)
 */

/**
 * @brief Wayland keyboard state (XKB handling)
 */
struct TypioWlKeyboard {
    struct zwp_input_method_keyboard_grab_v2 *grab;

    /* XKB state */
    struct xkb_context *xkb_context;
    struct xkb_keymap *xkb_keymap;
    struct xkb_state *xkb_state;

    /* XKB modifier indices (per-keyboard, not global) */
    xkb_mod_index_t mod_shift;
    xkb_mod_index_t mod_ctrl;
    xkb_mod_index_t mod_alt;
    xkb_mod_index_t mod_super;
    xkb_mod_index_t mod_caps;
    xkb_mod_index_t mod_num;
    uint32_t physical_modifiers;
    uint32_t mods_depressed;
    uint32_t mods_latched;
    uint32_t mods_locked;
    uint32_t mods_group;
    bool saw_blocking_modifier;

    /* Key repeat */
    int32_t repeat_rate;    /* Keys per second */
    int32_t repeat_delay;   /* Delay before repeat starts (ms) */
    int repeat_timer_fd;    /* timerfd for key repeat */
    uint32_t repeat_key;    /* Key currently repeating */
    uint32_t repeat_time;   /* Timestamp of the original press */
    bool repeating;         /* Whether a key is currently repeating */

    /* Grab epoch stamp: the Wayland dispatch epoch at which this grab was
     * created. The release path uses it to forward orphan releases of keys
     * that were physically held before this grab existed. Stale *presses*
     * are not filtered by a dispatch window — a key whose generation does
     * not match the active grab is caught by the generation fence instead. */
    uint64_t created_at_epoch;

    /* Key event arbiter for system shortcut detection */
    TypioKeyArbiter arbiter;

    /* Back reference */
    TypioWlFrontend *frontend;
};

/**
 * @brief Per-activation session state
 */
struct TypioWlSession {
    TypioInputContext *ctx;         /* Typio input context */

    /* Pending state (before done event) */
    struct {
        char *surrounding_text;
        uint32_t cursor;
        uint32_t anchor;
        uint32_t content_hint;
        uint32_t content_purpose;
        uint32_t text_change_cause;
        bool active;
    } pending;

    /* Current state (after done event) */
    struct {
        char *surrounding_text;
        uint32_t cursor;
        uint32_t anchor;
        uint32_t content_hint;
        uint32_t content_purpose;
    } current;

    /* Last preedit sent to app (for change detection) */
    char *last_preedit_text;
    int last_preedit_cursor;

    /* Latest snapshot from the composition callback. The new ABI delivers
     * candidates only through the callback, so the candidate-guard reads
     * this rather than querying the input context. */
    size_t last_candidate_count;
    int    last_candidate_selected;
    uint32_t last_host_managed_selection;

    /* Heap-owned deep copy of the latest composition's candidate list. The
     * libtypio borrowed pointers are only valid inside the callback, so we
     * copy everything we need to render later from the event loop. */
    TypioCandidateList candidate_snapshot;

    /* Back reference */
    TypioWlFrontend *frontend;
};

/**
 * @brief Main Wayland frontend state
 */
struct TypioWlFrontend {
    /* Core Typio instance */
    TypioInstance *instance;

    /* Wayland connection */
    struct wl_display *display;
    struct wl_registry *registry;
    struct wl_seat *seat;
    struct wl_compositor *compositor;
    struct wl_shm *shm;
    TypioWlOutput *outputs;

    /* Display name to reconnect to (strdup of the configured name, or NULL
     * for the default). Kept so the reconnect path can re-open the same
     * display after a compositor restart. */
    char *display_name;

    /* Input method protocol objects */
    struct zwp_input_method_manager_v2 *im_manager;
    struct zwp_input_method_v2 *input_method;

    /* Optional HiDPI helpers. Both may be nullptr on compositors that
     * predate the staging fractional-scale protocol or viewporter; the
     * panel falls back to integer wl_surface buffer_scale + wl_output
     * scale tracking when either is missing. */
    struct wp_fractional_scale_manager_v1 *fractional_scale_manager;
    struct wp_viewporter *viewporter;

    /* Virtual keyboard for forwarding unhandled keys */
    TypioWlVirtualKeyboard *vk;
    uint64_t guard_reject_press_streak;
    uint64_t guard_reject_press_window_start_ms;
    TypioWlWatchdog *watchdog;
    TypioWlKeyTracker *tracker;
    uint64_t trace_sequence;
    /* Session and keyboard state */
    TypioWlSession *session;
    TypioWlKeyboard *keyboard;
    TypioPanel *panel;
    TypioWlUiOwner ui_owner;
    TypioWlIdentityProvider *identity_provider;
    TypioWlIdentity current_identity;
    TypioEngineAvailability keyboard_availability;
    char keyboard_availability_reason[160];

    /* Optional subsystems registered as uniform TypioWlAuxHandler instances.
     * This replaces #ifdef-polluted struct members with a runtime array so
     * that the struct layout is stable across build configurations.
     * Capacity covers status_bus + tray + voice + resume_signal with
     * headroom. */
    TypioWlAuxHandler *aux_handlers[6];
    size_t aux_handler_count;

    /* System-resume detector (logind PrepareForSleep + boottime gap).
     * Always present; its DBus fd is polled via an aux handler while the
     * gap heuristic is ticked once per event-loop iteration. */
    TypioWlResumeSignal *resume_signal;

    /* IPC bus (UDS) — always available */
    struct TypioIpcBus *ipc_bus;

    /* Protocol serial: must increment on every done, even without a session.
     * This is the running count of zwp_input_method_v2 `done` events, which
     * is exactly the value commit() must echo back. */
    uint32_t im_serial;

    /* Serial passed to the most recent successful commit(); diagnostic
     * breadcrumb for trace output and a hook for future reconnect logic
     * that needs to know whether a staged commit was actually flushed. */
    uint32_t last_committed_serial;

    /* Wayland dispatch epoch — incremented after each wl_display_dispatch.
     * Used by the startup guard to deterministically identify stale key
     * presses re-sent by the compositor when a new keyboard grab is
     * established, replacing the previous time-based 50ms window. */
    uint64_t dispatch_epoch;

    /* Event loop state */
    volatile bool running;

    /* Session controller: raw input facts recorded this tick.
     * Written by input_method event handlers, consumed by the per-tick driver
     * in event_loop.c, then cleared. */
    TypioWlInputFacts session_facts;
    /* Previous tick's desired state, used by reduce() for edge detection
     * on focus_in / focus_out / reactivate. */
    TypioWlDesiredState session_prev_desired;

    TypioWlPanelCoordinator *panel_coord;
    TypioWlConfigWatcher *config;
    TypioWlIndicator *indicator;

    /* Configurable shortcut bindings */
    TypioShortcutConfig shortcuts;

    /* Error message buffer */
    char error_msg[256];
};

struct TypioWlPanelCoordinator {
    TypioWlPanelScheduleState panel_schedule_state;
    uint64_t position_anchor_generation;
    uint64_t position_anchor_ready_generation;
    uint64_t position_anchor_probe_generation;
    /* The compositor has positioned our popup at a caret for the current focus
     * (text_input_rectangle received). Survives anchor-generation resets; only
     * cleared on focus-out. Lets out-of-band UI fall back to the cached caret
     * rect when an anchor probe times out (terminals do not re-emit the rect on
     * the no-op probe commit, unlike browsers). */
    bool position_anchor_has_caret;
    bool positioned_ui_pending;
    TypioWlUiOwner positioned_ui_pending_owner;
    uint64_t positioned_ui_pending_since_ms;
    #define TYPIO_POSITIONED_UI_LABEL_CAP 256
    char positioned_ui_pending_label[TYPIO_POSITIONED_UI_LABEL_CAP];
    TypioWlFrontend *frontend;
};

struct TypioWlVirtualKeyboard {
    struct zwp_virtual_keyboard_manager_v1 *manager;
    struct zwp_virtual_keyboard_v1 *vk;
    TypioWlVirtualKeyboardState state;
    bool has_keymap;
    uint32_t keymap_generation;
    uint64_t drop_count;
    uint64_t keymap_cancel_count;
    uint64_t keymap_cancel_window_start_ms;
    uint64_t state_since_ms;
    uint64_t last_keymap_ms;
    uint64_t last_forward_ms;
    uint64_t keymap_deadline_ms;
    bool active_generation_dirty;
    bool carried_modifiers;
    TypioWlFrontend *frontend;
};

struct TypioWlWatchdog {
    TYPIO_ATOMIC(uint64_t) heartbeat_ms;
    TYPIO_ATOMIC(uint64_t) stage_since_ms;
    TYPIO_ATOMIC(bool) stop;
    TYPIO_ATOMIC(bool) armed;
    TYPIO_ATOMIC(int) loop_stage;
    pthread_t thread;
    bool thread_started;
    TypioWlFrontend *frontend;
};

struct TypioWlKeyTracker {
    TypioKeyTrackState states[TYPIO_WL_MAX_TRACKED_KEYS];
    uint32_t generations[TYPIO_WL_MAX_TRACKED_KEYS];
    uint32_t active_generation;
    bool active_generation_owned_keys;
    TypioWlFrontend *frontend;
};

struct TypioWlIndicator {
    int timer_fd;
    bool active;
    uint64_t last_key_activity_ms;
    uint64_t last_indicator_ms;
    #define TYPIO_INDICATOR_MODE_CACHE_CAP 8
    struct {
        char engine[64];
        char display_label[64];
    } mode_cache[TYPIO_INDICATOR_MODE_CACHE_CAP];
    size_t mode_cache_count;
    TypioWlFrontend *frontend;
};

struct TypioWlConfigWatcher {
    int watch_fd;
    int dir_watch;
    int engines_watch;
    int reload_timer_fd;
    bool reload_pending;
    TypioWlFrontend *frontend;
};

struct TypioWlOutput {
    TypioWlFrontend *frontend;
    uint32_t name;
    struct wl_output *output;
    int scale;
    TypioWlOutput *next;
};

void typio_wl_frontend_emit_runtime_state_changed(TypioWlFrontend *frontend);
void typio_wl_frontend_watchdog_heartbeat(TypioWlFrontend *frontend);
void typio_wl_frontend_watchdog_set_stage(TypioWlFrontend *frontend,
                                          TypioWlLoopStage stage);
void typio_wl_frontend_watchdog_start(TypioWlFrontend *frontend);
void typio_wl_frontend_watchdog_stop(TypioWlFrontend *frontend);
void typio_wl_frontend_log_shortcuts(TypioWlFrontend *frontend,
                                     const char *prefix);
void typio_wl_frontend_handle_config_watch(TypioWlFrontend *frontend);
int typio_wl_frontend_get_config_reload_fd(TypioWlFrontend *frontend);
void typio_wl_frontend_dispatch_config_reload(TypioWlFrontend *frontend);

/* Panel coordinator: owner arbitration and position-anchor policy. */
TypioPanelUpdateResult typio_wl_panel_coordinator_show_candidates(TypioWlFrontend *frontend,
                                                                  TypioInputContext *ctx);
bool typio_wl_panel_coordinator_show_status(TypioWlFrontend *frontend,
                                             TypioWlUiOwner owner,
                                             const char *text);
void typio_wl_panel_coordinator_hide(TypioWlFrontend *frontend,
                                      TypioWlUiOwner owner);
void typio_wl_panel_coordinator_hide_all(TypioWlFrontend *frontend);
void typio_wl_panel_coordinator_flush_pending(TypioWlFrontend *frontend);
void typio_wl_panel_coordinator_cancel_pending(TypioWlFrontend *frontend);
int typio_wl_panel_coordinator_anchor_timeout_ms(TypioWlFrontend *frontend);
void typio_wl_panel_coordinator_reset_anchor(TypioWlFrontend *frontend);
void typio_wl_panel_coordinator_mark_anchor_ready(TypioWlFrontend *frontend,
                                                   const char *reason);
bool typio_wl_panel_coordinator_anchor_ready(const TypioWlFrontend *frontend);
/* Record that the compositor delivered a caret rectangle for the current focus
 * (sets position_anchor_has_caret). Called from the input-popup surface. */
void typio_wl_panel_coordinator_note_caret_rect(TypioWlFrontend *frontend);
/* Clear the cached-caret state on focus-out so a later focus cannot reuse a
 * stale position. */
void typio_wl_panel_coordinator_clear_caret_rect(TypioWlFrontend *frontend);

bool typio_wl_frontend_init_indicator(TypioWlFrontend *frontend);
void typio_wl_frontend_show_indicator(TypioWlFrontend *frontend,
                                       const char *text);
void typio_wl_frontend_show_indicator_for_state(TypioWlFrontend *frontend,
                                                  const TypioKeyboardEngineMode *mode);
void typio_wl_frontend_show_indicator_on_focus(TypioWlFrontend *frontend,
                                                 const TypioKeyboardEngineMode *mode);
void typio_wl_frontend_record_key_activity(TypioWlFrontend *frontend);
void typio_wl_frontend_hide_indicator(TypioWlFrontend *frontend);
int typio_wl_frontend_get_indicator_fd(TypioWlFrontend *frontend);
void typio_wl_frontend_dispatch_indicator_timer(TypioWlFrontend *frontend);
void typio_wl_frontend_destroy_indicator(TypioWlFrontend *frontend);

/* Input method functions (input_method.c) */
void typio_wl_input_method_setup(TypioWlFrontend *frontend);

/* Reconnect after a lost Wayland display (frontend.c). Tears down and
 * re-establishes all Wayland objects with capped backoff, preserving engine
 * state. Returns true once reconnected, false if it gave up or was stopped. */
bool typio_wl_frontend_reconnect(TypioWlFrontend *frontend);

/* Session functions */
TypioWlSession *typio_wl_session_create(TypioWlFrontend *frontend);
void typio_wl_session_destroy(TypioWlSession *session);
void typio_wl_session_reset(TypioWlSession *session);
void typio_wl_session_apply_pending(TypioWlSession *session);
void typio_wl_session_flush_scheduled_ui_update(TypioWlSession *session);

/* Keyboard functions (keyboard.c) */
TypioWlKeyboard *typio_wl_keyboard_create(TypioWlFrontend *frontend);
void typio_wl_keyboard_destroy(TypioWlKeyboard *keyboard);
void typio_wl_keyboard_pause(TypioWlKeyboard *keyboard);
void typio_wl_keyboard_release_grab(TypioWlKeyboard *keyboard);
void typio_wl_keyboard_cancel_repeat(TypioWlKeyboard *keyboard);
int typio_wl_keyboard_get_repeat_fd(TypioWlKeyboard *keyboard);
void typio_wl_keyboard_dispatch_repeat(TypioWlKeyboard *keyboard);
void typio_wl_keyboard_process_key_press(TypioWlKeyboard *keyboard,
                                         TypioWlSession *session,
                                         uint32_t key, uint32_t keysym,
                                         uint32_t modifiers, uint32_t unicode,
                                         uint32_t time);
void typio_wl_keyboard_process_key_release(TypioWlKeyboard *keyboard,
                                           TypioWlSession *session,
                                           uint32_t key, uint32_t keysym,
                                           uint32_t modifiers, uint32_t unicode,
                                           uint32_t time);

/* Panel (candidate UI) public API lives in ui/panel/panel.h. */

/* Commit helpers */
void typio_wl_commit_string(TypioWlFrontend *frontend, const char *text);
void typio_wl_delete_surrounding(TypioWlFrontend *frontend,
                                 uint32_t before, uint32_t after);
void typio_wl_set_preedit(TypioWlFrontend *frontend, const char *text,
                          int cursor_begin, int cursor_end);
void typio_wl_commit(TypioWlFrontend *frontend);
void typio_wl_session_request_ui_update(TypioWlSession *session);
void typio_wl_session_cancel_ui_tracking(TypioWlSession *session);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_FRONTEND_INTERNAL_H */
