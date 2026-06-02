/**
 * @file key_route.c
 * @brief Key press/release routing for Wayland keyboard events
 */


#include "typio_build_config.h"
#include "router.h"

#ifdef HAVE_VOICE
#include "typio/abi/voice.h"
#endif

#include "boundary.h"
#include "candidate_guard.h"
#include "panel.h"
#include "preedit.h"
#include "debug.h"
#include "tracker_access.h"
#include "typio/abi/shortcut.h"
#include "chords.h"
#include "xkb.h"
#include "startup.h"
#include "bridge.h"
#include "trace.h"
#include "xkb_modifiers.h"
#include "recent_log.h"
#include "typio/runtime/instance.h"
#include "typio/runtime/registry.h"
#include "typio/abi/config.h"
#include "typio/typio.h"
#include "typio/abi/string.h"
#include "typio/abi/log.h"

#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <xkbcommon/xkbcommon-keysyms.h>

static TypioWlKeyDecision key_route_decision(TypioWlKeyAction action,
                                             TypioWlKeyReason reason) {
    TypioWlKeyDecision decision = {
        .action = action,
        .reason = reason,
    };
    return decision;
}

static void key_route_trace(TypioWlKeyboard *keyboard,
                            const char *stage,
                            uint32_t key,
                            uint32_t keysym,
                            uint32_t modifiers,
                            uint32_t unicode,
                            TypioKeyTrackState state,
                            const char *detail) {
    uint32_t xkb_mods = 0;
    char keysym_desc[64];
    char unicode_desc[64];

    if (!keyboard)
        return;

    if (keyboard->xkb_state)
        xkb_mods = typio_wl_xkb_effective_modifiers(keyboard);
    typio_wl_key_debug_format_keysym(keysym, keysym_desc, sizeof(keysym_desc));
    typio_wl_key_debug_format(unicode, unicode_desc, sizeof(unicode_desc));

    typio_wl_trace(keyboard->frontend,
                   "key",
                   "stage=%s keycode=%u keysym=0x%x %s route=%s mods=0x%x phys=0x%x xkb=0x%x keygen=%u activegen=%u %s detail=%s",
                   stage ? stage : "unknown",
                   key,
                   keysym,
                   keysym_desc,
                   typio_wl_key_tracking_state_name(state),
                   modifiers,
                   keyboard->physical_modifiers,
                   xkb_mods,
                   key < TYPIO_WL_MAX_TRACKED_KEYS ? keyboard->frontend->key_generations[key] : 0,
                   keyboard->frontend->active_key_generation,
                   unicode_desc,
                   detail ? detail : "-");
}

static void key_route_trace_decision(TypioWlKeyboard *keyboard,
                                     const char *stage,
                                     uint32_t key,
                                     uint32_t keysym,
                                     uint32_t modifiers,
                                     uint32_t unicode,
                                     TypioKeyTrackState state,
                                     TypioWlKeyDecision decision,
                                     const char *extra) {
    char detail[192];

    snprintf(detail, sizeof(detail),
             "action=%s reason=%s%s%s",
             typio_wl_key_action_name(decision.action),
             typio_wl_key_reason_name(decision.reason),
             extra && *extra ? " " : "",
             extra && *extra ? extra : "");
    key_route_trace(keyboard, stage, key, keysym, modifiers, unicode,
                    state, detail);
}

static bool key_route_is_app_shortcut(uint32_t keysym, uint32_t modifiers) {
    if ((modifiers & (TYPIO_MOD_CTRL | TYPIO_MOD_ALT | TYPIO_MOD_SUPER)) == 0)
        return false;

    switch (keysym) {
    case XKB_KEY_Shift_L:
    case XKB_KEY_Shift_R:
    case XKB_KEY_Control_L:
    case XKB_KEY_Control_R:
    case XKB_KEY_Alt_L:
    case XKB_KEY_Alt_R:
    case XKB_KEY_Super_L:
    case XKB_KEY_Super_R:
        return false;
    default:
        return true;
    }
}

/**
 * @brief Handle a host-managed candidate-selection key.
 *
 * Called when the candidate guard has decided to consume a key.  The host
 * updates its locally-managed selected index for navigation keys, or commits
 * the chosen candidate for commit/index-pick keys.
 */
static bool key_route_handle_host_selection(TypioWlFrontend *frontend,
                                            TypioWlSession *session,
                                            uint32_t keysym)
{
    TypioWlHostSelKey sel = typio_wl_host_selection_keysym(keysym);
    TypioWlHostSelCategory cat = typio_wl_host_selection_category(sel);

    if (cat == TYPIO_WL_HOST_SEL_CATEGORY_NAVIGATE) {
        int new_selected = typio_wl_host_selection_resolve(
            sel,
            session->last_candidate_selected,
            session->last_candidate_count);
        if (new_selected >= 0) {
            session->last_candidate_selected = new_selected;
            session->candidate_snapshot.selected = new_selected;
            session->frontend->panel_update_pending = true;
            typio_wl_session_flush_ui_update(session);
        }
        return true;
    }

    if (cat == TYPIO_WL_HOST_SEL_CATEGORY_COMMIT ||
        cat == TYPIO_WL_HOST_SEL_CATEGORY_INDEX_PICK) {
        return typio_wl_host_selection_try_commit(frontend, session, sel);
    }

    if (cat == TYPIO_WL_HOST_SEL_CATEGORY_COMMIT_RAW) {
        const TypioPreedit *preedit =
            typio_input_context_get_preedit(session->ctx);
        int cursor_pos = -1;
        char *plain_text = typio_wl_build_plain_preedit(preedit, &cursor_pos);
        typio_input_context_commit(session->ctx,
                                   plain_text ? plain_text : "");
        free(plain_text);
        return true;
    }

    return false;
}

const char *typio_wl_key_action_name(TypioWlKeyAction action) {
    switch (action) {
    case TYPIO_WL_KEY_ACTION_FORWARD:
        return "forward";
    case TYPIO_WL_KEY_ACTION_CONSUME:
    default:
        return "consume";
    }
}

const char *typio_wl_key_reason_name(TypioWlKeyReason reason) {
    switch (reason) {
    case TYPIO_WL_KEY_REASON_TYPIO_RESERVED:
        return "typio_reserved";
    case TYPIO_WL_KEY_REASON_APPLICATION_SHORTCUT:
        return "application_shortcut";
    case TYPIO_WL_KEY_REASON_BASIC_PASSTHROUGH:
        return "basic_passthrough";
    case TYPIO_WL_KEY_REASON_ENGINE_HANDLED:
        return "engine_handled";
    case TYPIO_WL_KEY_REASON_ENGINE_UNHANDLED:
        return "engine_unhandled";
    case TYPIO_WL_KEY_REASON_MODIFIER_PASSTHROUGH:
        return "modifier_passthrough";
    case TYPIO_WL_KEY_REASON_CANDIDATE_NAVIGATION:
        return "candidate_navigation";
    case TYPIO_WL_KEY_REASON_STARTUP_SUPPRESSED:
        return "startup_suppressed";
    case TYPIO_WL_KEY_REASON_RELEASED_PENDING:
        return "released_pending";
    case TYPIO_WL_KEY_REASON_LATCHED_APP_SHORTCUT:
        return "latched_app_shortcut";
    case TYPIO_WL_KEY_REASON_LATCHED_FORWARDED:
        return "latched_forwarded";
    case TYPIO_WL_KEY_REASON_STARTUP_STALE_CLEANUP:
        return "startup_stale_cleanup";
    case TYPIO_WL_KEY_REASON_FORWARDED_RELEASE:
        return "forwarded_release";
    case TYPIO_WL_KEY_REASON_ORPHAN_RELEASE_CLEANUP:
        return "orphan_release_cleanup";
    case TYPIO_WL_KEY_REASON_ORPHAN_RELEASE_CONSUMED:
        return "orphan_release_consumed";
    case TYPIO_WL_KEY_REASON_VOICE_PTT:
        return "voice_ptt";
    case TYPIO_WL_KEY_REASON_VOICE_PTT_UNAVAILABLE:
        return "voice_ptt_unavailable";
    case TYPIO_WL_KEY_REASON_NONE:
    default:
        return "none";
    }
}

const char *typio_wl_reserved_action_name(TypioWlReservedAction action) {
    switch (action) {
    case TYPIO_WL_RESERVED_ACTION_EMERGENCY_EXIT:
        return "emergency_exit";
    case TYPIO_WL_RESERVED_ACTION_VOICE_PTT:
        return "voice_ptt";
    case TYPIO_WL_RESERVED_ACTION_NONE:
    default:
        return "none";
    }
}

bool typio_wl_key_route_binding_matches_press(const TypioShortcutBinding *binding,
                                              uint32_t keysym,
                                              uint32_t modifiers) {
    if (!binding || binding->keysym == 0)
        return false;

    return keysym == binding->keysym &&
           (modifiers & binding->modifiers) == binding->modifiers;
}

TypioWlReservedAction typio_wl_key_route_reserved_action(
    const TypioShortcutConfig *shortcuts,
    uint32_t keysym,
    uint32_t modifiers) {
    if (!shortcuts)
        return TYPIO_WL_RESERVED_ACTION_NONE;

    if (typio_wl_key_route_binding_matches_press(&shortcuts->emergency_exit,
                                                 keysym, modifiers)) {
        return TYPIO_WL_RESERVED_ACTION_EMERGENCY_EXIT;
    }

    if (typio_wl_key_route_binding_matches_press(&shortcuts->voice_ptt,
                                                  keysym, modifiers)) {
        return TYPIO_WL_RESERVED_ACTION_VOICE_PTT;
    }

    return TYPIO_WL_RESERVED_ACTION_NONE;
}

static TypioWlKeyDecision key_route_shortcut_decision(
    const TypioShortcutConfig *shortcuts,
    uint32_t keysym,
    uint32_t modifiers,
    TypioWlReservedAction *reserved_action_out) {
    TypioWlReservedAction reserved_action =
        typio_wl_key_route_reserved_action(shortcuts, keysym, modifiers);

    if (reserved_action_out) {
        *reserved_action_out = reserved_action;
    }

    if (reserved_action != TYPIO_WL_RESERVED_ACTION_NONE) {
        return key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                  TYPIO_WL_KEY_REASON_TYPIO_RESERVED);
    }

    if (key_route_is_app_shortcut(keysym, modifiers)) {
        return key_route_decision(TYPIO_WL_KEY_ACTION_FORWARD,
                                  TYPIO_WL_KEY_REASON_APPLICATION_SHORTCUT);
    }

    return key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                              TYPIO_WL_KEY_REASON_NONE);
}

void typio_wl_key_route_process_press(TypioWlKeyboard *keyboard,
                                      TypioWlSession *session,
                                      uint32_t key,
                                      uint32_t keysym,
                                      uint32_t modifiers,
                                      uint32_t unicode,
                                      uint32_t time) {
    TypioWlFrontend *frontend = keyboard->frontend;
    TypioKeyTrackState kstate = key_get_state(frontend, key);
    TypioWlReservedAction reserved_action;
    TypioWlKeyDecision decision;

    if (kstate == TYPIO_KEY_TRACK_RELEASED_PENDING) {
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                      TYPIO_WL_KEY_REASON_RELEASED_PENDING);
        key_route_trace_decision(keyboard, "press-ignore", key, keysym,
                                 modifiers, unicode, kstate, decision, nullptr);
        typio_log_debug("Suppressing press for force-released key: keycode=%u", key);
        return;
    }

    if (kstate == TYPIO_KEY_TRACK_SUPPRESSED_STARTUP) {
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                      TYPIO_WL_KEY_REASON_STARTUP_SUPPRESSED);
        key_route_trace_decision(keyboard, "press-ignore", key, keysym,
                                 modifiers, unicode, kstate, decision,
                                 "repeat");
        typio_log_debug("Suppressing repeat of startup-guarded key: keycode=%u", key);
        return;
    }

    if (kstate == TYPIO_KEY_TRACK_APP_SHORTCUT) {
        if ((keyboard->physical_modifiers &
             (TYPIO_MOD_CTRL | TYPIO_MOD_ALT | TYPIO_MOD_SUPER)) != 0) {
            decision = key_route_decision(TYPIO_WL_KEY_ACTION_FORWARD,
                                          TYPIO_WL_KEY_REASON_LATCHED_APP_SHORTCUT);
            key_route_trace_decision(keyboard, "press-forward", key, keysym,
                                     modifiers, unicode, kstate, decision,
                                     nullptr);
            typio_wl_vk_forward_key(keyboard, time, key, WL_KEYBOARD_KEY_STATE_PRESSED, unicode);
        }
        typio_log_debug("Maintaining latched app shortcut route: keycode=%u", key);
        return;
    }

    if (kstate == TYPIO_KEY_TRACK_FORWARDED) {
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_FORWARD,
                                      TYPIO_WL_KEY_REASON_LATCHED_FORWARDED);
        key_route_trace_decision(keyboard, "press-forward", key, keysym,
                                 modifiers, unicode, kstate, decision, nullptr);
        typio_wl_vk_forward_key(keyboard, time, key, WL_KEYBOARD_KEY_STATE_PRESSED, unicode);
        typio_log_debug("Maintaining latched forwarded route: keycode=%u", key);
        return;
    }

    key_claim_current_generation(frontend, key);

    decision = key_route_shortcut_decision(&frontend->shortcuts,
                                           keysym, modifiers,
                                           &reserved_action);

    if (decision.reason != TYPIO_WL_KEY_REASON_NONE) {
        key_route_trace_decision(
            keyboard, "press-classify", key, keysym, modifiers,
            unicode, kstate, decision,
            decision.reason == TYPIO_WL_KEY_REASON_TYPIO_RESERVED
                ? typio_wl_reserved_action_name(reserved_action)
                : nullptr);
    }

    if (reserved_action == TYPIO_WL_RESERVED_ACTION_EMERGENCY_EXIT) {
        typio_log_warning(
                  "Emergency exit shortcut triggered: keycode=%u keysym=0x%x mods=0x%x",
                  key, keysym, modifiers);
        typio_dump_recent_log();
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                      TYPIO_WL_KEY_REASON_TYPIO_RESERVED);
        key_route_trace_decision(keyboard, "press-stop", key, keysym, modifiers,
                                 unicode, TYPIO_KEY_TRACK_IDLE, decision,
                                 "emergency_exit");
        typio_wl_keyboard_release_grab(keyboard);
        typio_wl_frontend_stop(frontend);
        return;
    }

#ifdef HAVE_VOICE
    TypioVoiceSession *voice = typio_instance_get_voice_session(frontend->instance);
    const TypioShortcutBinding *ptt = &frontend->shortcuts.voice_ptt;
    typio_log_debug("PTT check: keysym=0x%x mods=0x%x ptt_keysym=0x%x ptt_mods=0x%x voice_available=%s",
              keysym, modifiers, ptt->keysym, ptt->modifiers,
              typio_voice_session_is_available(voice) ? "yes" : "no");
    if (reserved_action == TYPIO_WL_RESERVED_ACTION_VOICE_PTT &&
        keysym == ptt->keysym &&
        (modifiers & ptt->modifiers) == ptt->modifiers) {
        if (typio_voice_session_is_available(voice)) {
            typio_voice_session_start(voice);
            key_set_state(frontend, key, TYPIO_KEY_TRACK_VOICE_PTT);
            decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                          TYPIO_WL_KEY_REASON_VOICE_PTT);
            key_route_trace_decision(keyboard, "press-ptt", key, keysym,
                                     modifiers, unicode, TYPIO_KEY_TRACK_VOICE_PTT,
                                     decision, "start");
            typio_log_debug("Voice PTT started: keycode=%u", key);
        } else {
            const char *reason =
                typio_voice_session_get_unavail_reason(voice);
            char hint[160];
            snprintf(hint, sizeof(hint), "[Voice unavailable: %s]",
                     reason ? reason : "unknown");
            key_set_state(frontend, key, TYPIO_KEY_TRACK_VOICE_PTT_UNAVAIL);
            typio_wl_panel_coordinator_show_status(frontend, TYPIO_WL_UI_OWNER_VOICE, hint);
            decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                          TYPIO_WL_KEY_REASON_VOICE_PTT_UNAVAILABLE);
            key_route_trace_decision(keyboard, "press-ptt-unavail", key, keysym,
                                     modifiers, unicode,
                                     TYPIO_KEY_TRACK_VOICE_PTT_UNAVAIL,
                                     decision, "not_available");
            typio_log_warning("Voice PTT pressed but unavailable: %s",
                      reason ? reason : "unknown");
        }
        return;
    }
#endif

    if (decision.reason == TYPIO_WL_KEY_REASON_APPLICATION_SHORTCUT) {
        TypioKeyEvent event = {
            .struct_size = sizeof(TypioKeyEvent),
            .type      = TYPIO_EVENT_KEY_PRESS,
            .keycode   = key,
            .keysym    = keysym,
            .modifiers = modifiers,
            .unicode   = unicode,
            .time      = time,
            .is_repeat = false,
            .base_keysym = typio_wl_keyboard_base_keysym(keyboard, key),
        };
        bool handled = typio_input_context_process_key(session->ctx, &event);
        typio_log_info("app-shortcut engine try: keysym=0x%x base=0x%x mods=0x%x handled=%s",
                       keysym, event.base_keysym, modifiers,
                       handled ? "yes" : "no");
        if (handled) {
            key_set_state(frontend, key, TYPIO_KEY_TRACK_IDLE);
            key_route_trace_decision(keyboard, "press-engine", key, keysym,
                                     modifiers, unicode, TYPIO_KEY_TRACK_IDLE,
                                     decision, "app-shortcut-intercepted-by-engine");
            typio_log_debug("Engine handled potential app shortcut: keycode=%u keysym=0x%x mods=0x%x",
                      key, keysym, modifiers);
            return;
        }
        typio_wl_vk_forward_key(keyboard, time, key, WL_KEYBOARD_KEY_STATE_PRESSED, unicode);
        key_set_state(frontend, key, TYPIO_KEY_TRACK_APP_SHORTCUT);
        key_route_trace_decision(keyboard, "press-forward", key, keysym,
                                 modifiers, unicode, TYPIO_KEY_TRACK_APP_SHORTCUT,
                                 decision, nullptr);
        typio_log_debug("Bypassing engine for application shortcut: keycode=%u keysym=0x%x mods=0x%x",
                  key, keysym, modifiers);
        return;
    }

    {
        TypioKeyEvent event = {
            .struct_size = sizeof(TypioKeyEvent),
            .type      = TYPIO_EVENT_KEY_PRESS,
            .keycode   = key,
            .keysym    = keysym,
            .modifiers = modifiers,
            .unicode   = unicode,
            .time      = time,
            .is_repeat = false,
            .base_keysym = typio_wl_keyboard_base_keysym(keyboard, key),
        };
        bool is_modifier = typio_key_event_is_modifier_only(&event);
        bool handled = typio_input_context_process_key(session->ctx, &event);

        if (!handled &&
            typio_wl_candidate_guard_should_consume(session, keysym)) {
            key_route_handle_host_selection(frontend, session, keysym);
            decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                          TYPIO_WL_KEY_REASON_CANDIDATE_NAVIGATION);
            key_route_trace_decision(keyboard, "press-engine", key, keysym,
                                     modifiers, unicode, TYPIO_KEY_TRACK_IDLE,
                                     decision, nullptr);
            typio_log_debug("Reserved navigation key for candidate UI");
        } else if (!handled) {
            typio_wl_vk_forward_key(keyboard, time, key, WL_KEYBOARD_KEY_STATE_PRESSED, unicode);
            key_set_state(frontend, key, TYPIO_KEY_TRACK_FORWARDED);
            decision = key_route_decision(
                TYPIO_WL_KEY_ACTION_FORWARD,
                is_modifier ? TYPIO_WL_KEY_REASON_MODIFIER_PASSTHROUGH
                            : TYPIO_WL_KEY_REASON_ENGINE_UNHANDLED);
            key_route_trace_decision(keyboard, "press-forward", key, keysym,
                                     modifiers, unicode, TYPIO_KEY_TRACK_FORWARDED,
                                     decision, nullptr);
            typio_log_debug("Key forwarded to application");
        } else {
            decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                          TYPIO_WL_KEY_REASON_ENGINE_HANDLED);
            key_route_trace_decision(keyboard, "press-engine", key, keysym,
                                     modifiers, unicode, TYPIO_KEY_TRACK_IDLE,
                                     decision, nullptr);
            typio_log_debug("Key handled by input method");
        }
    }
}

void typio_wl_key_route_process_release(TypioWlKeyboard *keyboard,
                                        TypioWlSession *session,
                                        uint32_t key,
                                        uint32_t keysym,
                                        uint32_t modifiers,
                                        uint32_t unicode,
                                        uint32_t time) {
    TypioWlFrontend *frontend = keyboard->frontend;
    TypioKeyTrackState kstate = key_get_state(frontend, key);
    TypioWlKeyDecision decision;

    if (keyboard->repeating && keyboard->repeat_key == key)
        keyboard->repeat_timer_fd >= 0 ? (void)0 : (void)0;

    switch (kstate) {
    case TYPIO_KEY_TRACK_RELEASED_PENDING:
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                      TYPIO_WL_KEY_REASON_RELEASED_PENDING);
        key_route_trace_decision(keyboard, "release-consume", key, keysym,
                                 modifiers, unicode, kstate, decision, nullptr);
        key_clear_tracking(frontend, key);
        typio_log_debug("Consumed physical release for force-released key: keycode=%u",
                  key);
        return;

    case TYPIO_KEY_TRACK_SUPPRESSED_STARTUP:
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_FORWARD,
                                      TYPIO_WL_KEY_REASON_STARTUP_STALE_CLEANUP);
        key_route_trace_decision(keyboard, "release-forward", key, keysym,
                                 modifiers, unicode, kstate, decision, nullptr);
        key_clear_tracking(frontend, key);
        typio_wl_vk_forward_key(keyboard, time, key, WL_KEYBOARD_KEY_STATE_RELEASED, unicode);
        typio_log_debug("Forwarding release for startup key: keycode=%u", key);
        return;

    case TYPIO_KEY_TRACK_APP_SHORTCUT:
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_FORWARD,
                                      TYPIO_WL_KEY_REASON_APPLICATION_SHORTCUT);
        key_route_trace_decision(keyboard, "release-forward", key, keysym,
                                 modifiers, unicode, kstate, decision,
                                 "release");
        key_clear_tracking(frontend, key);
        typio_wl_vk_forward_key(keyboard, time, key, WL_KEYBOARD_KEY_STATE_RELEASED, unicode);
        typio_log_debug("Forwarded application shortcut release: keycode=%u", key);
        return;

    case TYPIO_KEY_TRACK_VOICE_PTT:
#ifdef HAVE_VOICE
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                      TYPIO_WL_KEY_REASON_VOICE_PTT);
        key_route_trace_decision(keyboard, "release-ptt", key, keysym,
                                 modifiers, unicode, kstate, decision, "stop");
        {
            TypioVoiceSession *voice =
                typio_instance_get_voice_session(frontend->instance);
            typio_voice_session_stop(voice);
        }
        key_clear_tracking(frontend, key);
        typio_log_debug("Voice PTT released: keycode=%u", key);
        return;
#else
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                      TYPIO_WL_KEY_REASON_VOICE_PTT);
        key_route_trace_decision(keyboard, "release-consume", key, keysym,
                                 modifiers, unicode, kstate, decision,
                                 "unsupported_build");
        key_clear_tracking(frontend, key);
        return;
#endif

    case TYPIO_KEY_TRACK_VOICE_PTT_UNAVAIL:
#ifdef HAVE_VOICE
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                      TYPIO_WL_KEY_REASON_VOICE_PTT_UNAVAILABLE);
        key_route_trace_decision(keyboard, "release-ptt-unavail", key, keysym,
                                 modifiers, unicode, kstate, decision,
                                 "release");
        typio_wl_panel_coordinator_hide(frontend, TYPIO_WL_UI_OWNER_VOICE);
        key_clear_tracking(frontend, key);
        typio_log_debug("Voice PTT unavail released: keycode=%u", key);
        return;
#else
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                      TYPIO_WL_KEY_REASON_VOICE_PTT_UNAVAILABLE);
        key_route_trace_decision(keyboard, "release-consume", key, keysym,
                                 modifiers, unicode, kstate, decision,
                                 "unsupported_build");
        key_clear_tracking(frontend, key);
        return;
#endif

    case TYPIO_KEY_TRACK_FORWARDED:
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_FORWARD,
                                      TYPIO_WL_KEY_REASON_FORWARDED_RELEASE);
        key_route_trace_decision(keyboard, "release-forward", key, keysym,
                                 modifiers, unicode, kstate, decision, nullptr);
        key_clear_tracking(frontend, key);
        break;

    case TYPIO_KEY_TRACK_BASIC_PASSTHROUGH:
        decision = key_route_decision(TYPIO_WL_KEY_ACTION_FORWARD,
                                      TYPIO_WL_KEY_REASON_FORWARDED_RELEASE);
        key_route_trace_decision(keyboard, "release-forward", key, keysym,
                                 modifiers, unicode, kstate, decision,
                                 "engine=basic");
        key_clear_tracking(frontend, key);
        typio_wl_vk_forward_key(keyboard, time, key,
                                WL_KEYBOARD_KEY_STATE_RELEASED, unicode);
        return;

    case TYPIO_KEY_TRACK_IDLE:
        if (!key_owned_by_active_generation(frontend, key)) {
            TypioKeyEvent release_event = {
                .type      = TYPIO_EVENT_KEY_RELEASE,
                .keycode   = key,
                .keysym    = keysym,
                .modifiers = modifiers,
                .unicode   = unicode,
                .time      = time,
                .is_repeat = false,
                .base_keysym = typio_wl_keyboard_base_keysym(keyboard, key),
            };
            bool is_modifier = typio_key_event_is_modifier_only(&release_event);
            bool should_cleanup_stale_release =
                !is_modifier &&
                (typio_wl_startup_guard_is_in_guard_window(
                 keyboard->created_at_epoch, frontend->dispatch_epoch) ||
                 typio_wl_boundary_bridge_should_forward_orphan_release_cleanup(
                     keysym,
                     modifiers,
                     keyboard->saw_blocking_modifier));

            if (should_cleanup_stale_release) {
                decision = key_route_decision(
                    TYPIO_WL_KEY_ACTION_FORWARD,
                    TYPIO_WL_KEY_REASON_ORPHAN_RELEASE_CLEANUP);
                key_route_trace_decision(keyboard, "release-forward", key, keysym,
                                         modifiers, unicode, kstate, decision,
                                         nullptr);
                typio_wl_vk_forward_key(keyboard, time, key,
                                        WL_KEYBOARD_KEY_STATE_RELEASED,
                                        unicode);
                typio_log_debug("Forwarded orphan release for pre-grab key: keycode=%u",
                          key);
                key_clear_tracking(frontend, key);
                return;
            }

            decision = key_route_decision(
                TYPIO_WL_KEY_ACTION_CONSUME,
                TYPIO_WL_KEY_REASON_ORPHAN_RELEASE_CONSUMED);
            key_route_trace_decision(keyboard, "release-orphan", key, keysym,
                                     modifiers, unicode, kstate, decision,
                                     "press_never_reached_typio");
            typio_log_debug("Consumed orphan release: keycode=%u generation=%u active_generation=%u",
                      key, key_get_generation(frontend, key),
                      frontend->active_key_generation);
            key_clear_tracking(frontend, key);
            return;
        }

        decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                      TYPIO_WL_KEY_REASON_ENGINE_HANDLED);
        key_route_trace_decision(keyboard, "release-engine", key, keysym,
                                 modifiers, unicode, kstate, decision,
                                 "idle_release");
        {
            TypioKeyEvent ev = {
                .type      = TYPIO_EVENT_KEY_RELEASE,
                .keycode   = key,
                .keysym    = keysym,
                .modifiers = modifiers,
                .unicode   = unicode,
                .time      = time,
                .is_repeat = false,
                .base_keysym = typio_wl_keyboard_base_keysym(keyboard, key),
            };
            bool handled = typio_input_context_process_key(session->ctx, &ev);
            if (!handled &&
                typio_wl_candidate_guard_should_consume(session, keysym)) {
                decision = key_route_decision(
                    TYPIO_WL_KEY_ACTION_CONSUME,
                    TYPIO_WL_KEY_REASON_CANDIDATE_NAVIGATION);
                key_route_trace_decision(keyboard, "release-engine", key, keysym,
                                         modifiers, unicode, kstate, decision,
                                         nullptr);
            }
        }
        key_clear_tracking(frontend, key);
        return;
    }

    {
        TypioKeyEvent event = {
            .struct_size = sizeof(TypioKeyEvent),
            .type      = TYPIO_EVENT_KEY_RELEASE,
            .keycode   = key,
            .keysym    = keysym,
            .modifiers = modifiers,
            .unicode   = unicode,
            .time      = time,
            .is_repeat = false,
            .base_keysym = typio_wl_keyboard_base_keysym(keyboard, key),
        };
        bool handled = typio_input_context_process_key(session->ctx, &event);
        if (!handled &&
            typio_wl_candidate_guard_should_consume(session, keysym)) {
            decision = key_route_decision(TYPIO_WL_KEY_ACTION_CONSUME,
                                          TYPIO_WL_KEY_REASON_CANDIDATE_NAVIGATION);
            key_route_trace_decision(keyboard, "release-engine", key, keysym,
                                     modifiers, unicode, TYPIO_KEY_TRACK_IDLE,
                                     decision, nullptr);
            key_clear_tracking(frontend, key);
            return;
        }
    }

    typio_wl_vk_forward_key(keyboard, time, key, WL_KEYBOARD_KEY_STATE_RELEASED, unicode);
}
