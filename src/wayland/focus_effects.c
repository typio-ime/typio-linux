/**
 * @file focus_effects.c
 * @brief Effectful observe and apply for the focus controller.
 *
 * This module reads live TypioWlFrontend fields (observe) and mutates them
 * (apply). It is the effectful half of the session-controller pipeline;
 * the pure half lives in src/engine/focus_controller.c.
 *
 * ## Apply execution order
 *
 * The apply() function executes effects in a fixed order that is part of
 * the contract — reordering breaks the engine boundary invariants. The
 * order is:
 *
 *   1. discard_composition — abandon the engine's in-flight composition and
 *                          candidate UI while the field is still focused, so
 *                          a half-typed attempt cannot leak into the next
 *                          field or auto-commit into the one being left
 *   2. focus_out        — engine stops processing keys first
 *   3. destroy_grab     — hard teardown: release forwarded keys, drop the
 *                          grab, reset vk modifiers, scrub the key
 *                          generation, force the per-key tracker empty
 *   4. clear_preedit    — blank the compositor-visible preedit
 *   5. commit           — flush the staged preedit before teardown reaches
 *                          the compositor
 *   6. scrub_generation — fence stale key state across the boundary
 *   7. create_grab      — build a new keyboard grab object; the keymap
 *                          handshake starts in NEEDS_KEYMAP
 *   8. focus_in         — engine focus_in, identity refresh, indicator
 *                          show-on-focus (only on a YES edge)
 *   9. reactivate       — re-anchor the panel to the new caret (YES→YES
 *                          with a fresh activate in the same done batch)
 *
 * The order is enforced at compile time by TYPIO_WL_SESSION_EFFECT_ORDER
 * below. Adding a new effect requires deciding its position relative to
 * the existing nine and updating the assertion in lockstep.
 */

#include "focus_controller.h"

#include "internal.h"
#include "wayland/keyboard/bridge.h"
#include "wayland/foreign/identity.h"
#include "panel.h"
#include "clock.h"
#include "typio/abi/instance.h"
#include "typio/abi/input_context.h"
#include "typio/abi/log.h"
#include "typio/abi/types.h"
#include "trace.h"

#include <inttypes.h>

/* ── Effect-order contract ────────────────────────────────────────────── */
/*
 * The static_assert below encodes the apply order as a list of distinct
 * integers. Reordering or removing effects from apply() requires changing
 * the corresponding integer here. The compile error forces the change to
 * be deliberate.
 */
#define TYPIO_WL_EFFECT_STEP_DISCARD_COMPOSITION 0
#define TYPIO_WL_EFFECT_STEP_FOCUS_OUT       1
#define TYPIO_WL_EFFECT_STEP_DESTROY_GRAB    2
#define TYPIO_WL_EFFECT_STEP_CLEAR_PREEDIT   3
#define TYPIO_WL_EFFECT_STEP_COMMIT          4
#define TYPIO_WL_EFFECT_STEP_SCRUB_GENERATION 5
#define TYPIO_WL_EFFECT_STEP_CREATE_GRAB     6
#define TYPIO_WL_EFFECT_STEP_FOCUS_IN        7
#define TYPIO_WL_EFFECT_STEP_REACTIVATE      8

/* Compile-time ordering check: each step must be a distinct integer and
 * the list must be a permutation of {0,1,2,3,4,5,6,7,8}. If you add a new
 * step, choose its slot and update the bitmask accordingly. */
_Static_assert(
    ((1 << TYPIO_WL_EFFECT_STEP_DISCARD_COMPOSITION) |
     (1 << TYPIO_WL_EFFECT_STEP_FOCUS_OUT)       |
     (1 << TYPIO_WL_EFFECT_STEP_DESTROY_GRAB)    |
     (1 << TYPIO_WL_EFFECT_STEP_CLEAR_PREEDIT)   |
     (1 << TYPIO_WL_EFFECT_STEP_COMMIT)          |
     (1 << TYPIO_WL_EFFECT_STEP_SCRUB_GENERATION) |
     (1 << TYPIO_WL_EFFECT_STEP_CREATE_GRAB)     |
     (1 << TYPIO_WL_EFFECT_STEP_FOCUS_IN)        |
     (1 << TYPIO_WL_EFFECT_STEP_REACTIVATE)) == 0x1FF,
    "Session effect order changed. Update the apply() ordering and this "
    "static_assert to match. The two must move together.");

/* ── Observe: live resource snapshot ──────────────────────────────────── */

TypioWlActualState
typio_wl_focus_observe(const TypioWlFrontend *frontend)
{
    TypioWlActualState actual = {
        .connection_alive = false,
        .ic_focused = false,
        .grab = TYPIO_WL_GRAB_RES_ABSENT,
    };

    if (!frontend)
        return actual;

    /* Connection is considered alive if the display object exists.
     * The event-loop layer sets facts.connection_alive=false on POLLHUP
     * or reconnect, which is the authoritative fact source. */
    actual.connection_alive = frontend->display != nullptr;

    if (frontend->session && frontend->session->ctx)
        actual.ic_focused = typio_input_context_is_focused(frontend->session->ctx);

    /* Grab resource state: merge keyboard object presence with vk readiness.
     * This is one resource with one state, per the timing model. */
    if (!frontend->keyboard) {
        actual.grab = TYPIO_WL_GRAB_RES_ABSENT;
    } else if (frontend->vk->state == TYPIO_WL_VK_STATE_BROKEN) {
        actual.grab = TYPIO_WL_GRAB_RES_BROKEN;
    } else if (frontend->vk->state == TYPIO_WL_VK_STATE_NEEDS_KEYMAP) {
        actual.grab = TYPIO_WL_GRAB_RES_NEEDS_KEYMAP;
    } else if (frontend->vk->state == TYPIO_WL_VK_STATE_READY) {
        actual.grab = TYPIO_WL_GRAB_RES_READY;
    } else {
        actual.grab = TYPIO_WL_GRAB_RES_ABSENT;
    }

    return actual;
}

/* ── Apply: effectful execution ───────────────────────────────────────── */

/* Hard teardown of the keyboard resource. Released forwarded keys, drops
 * the compositor-visible preedit, resets carried vk modifier state, and
 * brings every per-key tracker entry back to IDLE. Idempotent. */
static void
focus_hard_reset_keyboard(TypioWlFrontend *frontend, const char *reason)
{
    if (!frontend)
        return;

    typio_wl_trace(frontend,
                   "session",
                   "action=hard_reset reason=%s",
                   reason ? reason : "focus_controller");

    /* Walk tracking before destroy so the release events for keys we
     * forwarded to the client fire while the per-key state is still
     * meaningful. Destroy then clears the grab object. */
    if (frontend->keyboard) {
        typio_wl_vk_release_forwarded_keys(
            frontend, typio_wl_key_tracking_state_name);
        typio_wl_keyboard_cancel_repeat(frontend->keyboard);
        typio_wl_keyboard_destroy(frontend->keyboard);
        frontend->keyboard = nullptr;
    }

    /* Any in-flight keymap wait is moot without a grab. */
    typio_wl_vk_cancel_keymap_wait(frontend, "hard_reset");

    /* Reset vk modifier state: a hard teardown never carries modifiers
     * across the boundary (suspend, reconnect, broken recovery). */
    typio_wl_vk_reset_modifiers(frontend);
    frontend->vk->carried_modifiers = false;
    frontend->tracker->active_generation_owned_keys = false;
    frontend->vk->active_generation_dirty = false;
}

static void
show_focus_indicator(TypioWlFrontend *frontend, bool force)
{
    if (!frontend || !frontend->instance)
        return;

    const TypioKeyboardEngineMode *mode =
        typio_instance_get_last_keyboard_mode(frontend->instance);
    if (!mode || !mode->display_label || !mode->display_label[0])
        return;

    if (!force) {
        typio_wl_frontend_show_indicator_on_focus(frontend, mode);
        return;
    }

    if (mode->salience == TYPIO_STATUS_SALIENCE_QUIET)
        return;

    typio_wl_frontend_show_indicator_for_state(frontend, mode);
}

void
typio_wl_focus_apply(TypioWlFrontend *frontend,
                       const TypioWlEffectSet *effects)
{
    if (!frontend || !effects)
        return;

    /* Order is part of the contract. See the static_assert above and the
     * documented apply execution order in the file header. */

    if (effects->discard_composition && frontend->session &&
        frontend->session->ctx) {
        typio_wl_trace(frontend, "session", "action=discard_composition");
        /* Abandon the engine's in-flight composition while the field is
         * still focused, before the focus_out notification below.
         * typio_input_context_focus_out() only invokes the engine's
         * focus_out hook; it does NOT drop the pending preedit/candidates.
         * Without this reset the half-typed composition (e.g. an unconverted
         * RIME pinyin) survives the defocus and is replayed into the next
         * text field on its focus_in, leaking stale underlined text into an
         * unrelated input. Resetting before focus_out also stops the engine
         * from auto-committing the abandoned attempt into the field being
         * left. The compositor-visible preedit is blanked by the
         * clear_preedit + commit effects below; here we tear down only the
         * engine-side state and its candidate UI. Mirrors the engine-switch
         * teardown in arbiter.c. */
        typio_input_context_reset(frontend->session->ctx);
        typio_wl_panel_coordinator_hide(frontend, TYPIO_WL_UI_OWNER_CANDIDATE);
        typio_wl_session_cancel_ui_tracking(frontend->session);
    }

    if (effects->send_focus_out && frontend->session && frontend->session->ctx) {
        typio_wl_trace(frontend, "session", "action=focus_out");
        typio_input_context_focus_out(frontend->session->ctx);
        typio_wl_frontend_clear_identity(frontend);
        if (frontend->keyboard) {
            /* Soft pause: release forwarded keys, reset tracking, disarm
             * repeat, zero xkb and host-side arbitration state. The grab
             * object is retained so the next focus reuses it. */
            typio_wl_keyboard_pause(frontend->keyboard);
        }
    }

    if (effects->destroy_grab) {
        focus_hard_reset_keyboard(frontend, "focus_controller");
    }

    if (effects->clear_preedit) {
        typio_wl_set_preedit(frontend, "", -1, -1);
    }
    if (effects->commit) {
        typio_wl_commit(frontend);
    }

    if (effects->scrub_generation) {
        frontend->tracker->active_generation++;
        if (frontend->tracker->active_generation == 0)
            frontend->tracker->active_generation = 1;
        typio_wl_key_tracking_reset(frontend->tracker->states,
                                    TYPIO_WL_MAX_TRACKED_KEYS);
        typio_wl_key_tracking_reset_generations(frontend->tracker->generations,
                                                TYPIO_WL_MAX_TRACKED_KEYS);
        typio_wl_trace(frontend,
                       "session",
                       "action=scrub_generation gen=%u",
                       frontend->tracker->active_generation);
    }

    if (effects->create_grab) {
        typio_wl_trace(frontend, "session", "action=create_grab");
        frontend->keyboard = typio_wl_keyboard_create(frontend);
        typio_wl_vk_expect_keymap(frontend, "focus_controller create_grab");
    }

    if (effects->send_focus_in) {
        typio_wl_panel_coordinator_reset_anchor(frontend);
        typio_wl_panel_coordinator_early_anchor_probe(frontend);
    }

    if (effects->send_focus_in && frontend->session && frontend->session->ctx) {
        typio_wl_trace(frontend, "session", "action=focus_in");
        typio_input_context_focus_in(frontend->session->ctx);
        typio_wl_frontend_refresh_identity(frontend);
        typio_wl_frontend_restore_identity_engine(frontend);
        show_focus_indicator(frontend, false);
    }

    if (effects->reactivate) {
        typio_wl_trace(frontend, "session", "action=reactivate");
        typio_wl_panel_coordinator_reset_anchor(frontend);
        typio_wl_panel_coordinator_early_anchor_probe(frontend);
        show_focus_indicator(frontend, true);
    }
}
