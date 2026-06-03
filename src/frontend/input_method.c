/**
 * @file input_method.c
 * @brief zwp_input_method_v2 event handlers
 */

#include "internal.h"
#include "panel.h"
#include "identity.h"
#include "monotonic.h"
#include "preedit.h"
#include "state.h"
#include "trace.h"
#include "typio/runtime/instance.h"
#include "typio/runtime/registry.h"
#include "typio/typio.h"
#include "typio/abi/log.h"
#include "typio/abi/string.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define TYPIO_WL_UI_SLOW_UPDATE_MS 8

/* Forward declarations for callbacks */
static void on_commit_callback(TypioInputContext *ctx, const char *text,
                               void *user_data);
static void on_composition_callback(TypioInputContext *ctx,
                                    const TypioComposition *composition,
                                    void *user_data);
static void on_delete_surrounding_callback(TypioInputContext *ctx,
                                           uint32_t before, uint32_t after,
                                           void *user_data);
static void update_wayland_text_ui(TypioWlSession *session, TypioInputContext *ctx);

static char *typio_dup_or_null(const char *s) {
    return s ? typio_strdup(s) : nullptr;
}

static void candidate_snapshot_clear(TypioCandidateList *snap) {
    if (!snap)
        return;
    for (size_t i = 0; i < snap->count; i++) {
        free((char *)snap->candidates[i].text);
        free((char *)snap->candidates[i].comment);
        free((char *)snap->candidates[i].label);
    }
    free(snap->candidates);
    snap->candidates = nullptr;
    snap->count = 0;
    snap->selected = -1;
    snap->total = 0;
    snap->page = 0;
    snap->page_size = 0;
    snap->has_prev = false;
    snap->has_next = false;
    snap->content_signature = 0;
}

static bool candidate_snapshot_equal_content(const TypioCandidateList *snap,
                                              const TypioComposition *comp)
{
    if (!snap || !comp) return false;
    if (snap->count != comp->candidate_count) return false;
    if (snap->page != comp->page || snap->page_size != comp->page_size) return false;
    if (snap->total != comp->total) return false;
    if (snap->has_prev != comp->has_prev || snap->has_next != comp->has_next) return false;
    for (size_t i = 0; i < snap->count; i++) {
        const char *t1 = snap->candidates[i].text    ? snap->candidates[i].text    : "";
        const char *t2 = comp->candidates[i].text    ? comp->candidates[i].text    : "";
        const char *c1 = snap->candidates[i].comment ? snap->candidates[i].comment : "";
        const char *c2 = comp->candidates[i].comment ? comp->candidates[i].comment : "";
        const char *l1 = snap->candidates[i].label   ? snap->candidates[i].label   : "";
        const char *l2 = comp->candidates[i].label   ? comp->candidates[i].label   : "";
        if (strcmp(t1, t2) != 0 || strcmp(c1, c2) != 0 || strcmp(l1, l2) != 0)
            return false;
    }
    return true;
}

static void candidate_snapshot_assign(TypioCandidateList *snap,
                                      const TypioComposition *composition) {
    if (!composition || composition->candidate_count == 0) {
        candidate_snapshot_clear(snap);
        return;
    }

    /* Fast path: only the selected highlight moved.  Skip the expensive
     * clear + calloc + strdup round-trip.  This is the common case when
     * the user pages through RIME candidates with Up/Down. */
    if (candidate_snapshot_equal_content(snap, composition)) {
        snap->selected = composition->selected;
        snap->content_signature = composition->content_signature;
        return;
    }

    candidate_snapshot_clear(snap);
    TypioCandidate *items = calloc(composition->candidate_count,
                                    sizeof(TypioCandidate));
    if (!items) {
        return;
    }
    for (size_t i = 0; i < composition->candidate_count; i++) {
        items[i].text    = typio_dup_or_null(composition->candidates[i].text);
        items[i].comment = typio_dup_or_null(composition->candidates[i].comment);
        items[i].label   = typio_dup_or_null(composition->candidates[i].label);
    }
    snap->candidates = items;
    snap->count = composition->candidate_count;
    snap->selected = composition->selected;
    snap->total = composition->total;
    snap->page = composition->page;
    snap->page_size = composition->page_size;
    snap->has_prev = composition->has_prev;
    snap->has_next = composition->has_next;
    snap->content_signature = composition->content_signature;
}

/* Reset all candidate-session state derived from a composition: the
 * candidate-guard scalars consulted on every key and the heap-owned snapshot
 * used to re-render the panel. libtypio's commit is silent (it clears the
 * in-flight composition without firing the composition callback; see
 * typio_input_context_commit), so the commit callback must run this teardown
 * itself. The composition callback's empty branch funnels through here too, so
 * both "engine cleared the composition" and "text was committed" reach the same
 * idle state. */
static void session_clear_candidate_state(TypioWlSession *session) {
    if (!session)
        return;
    session->last_candidate_count = 0;
    session->last_candidate_selected = -1;
    session->last_host_managed_selection = TYPIO_HOST_SEL_NONE;
    candidate_snapshot_clear(&session->candidate_snapshot);
}

/* Input method event handlers */
static void im_handle_activate(void *data, struct zwp_input_method_v2 *im);
static void im_handle_deactivate(void *data, struct zwp_input_method_v2 *im);
static void im_handle_surrounding_text(void *data, struct zwp_input_method_v2 *im,
                                       const char *text, uint32_t cursor,
                                       uint32_t anchor);
static void im_handle_text_change_cause(void *data, struct zwp_input_method_v2 *im,
                                        uint32_t cause);
static void im_handle_content_type(void *data, struct zwp_input_method_v2 *im,
                                   uint32_t hint, uint32_t purpose);
static void im_handle_done(void *data, struct zwp_input_method_v2 *im);
static void im_handle_unavailable(void *data, struct zwp_input_method_v2 *im);

static const struct zwp_input_method_v2_listener input_method_listener = {
    .activate = im_handle_activate,
    .deactivate = im_handle_deactivate,
    .surrounding_text = im_handle_surrounding_text,
    .text_change_cause = im_handle_text_change_cause,
    .content_type = im_handle_content_type,
    .done = im_handle_done,
    .unavailable = im_handle_unavailable,
};

void typio_wl_input_method_setup(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->input_method) {
        return;
    }
    zwp_input_method_v2_add_listener(frontend->input_method,
                                     &input_method_listener, frontend);
}

static bool session_is_focused(TypioWlFrontend *frontend) {
    return frontend && frontend->session && frontend->session->ctx &&
           typio_input_context_is_focused(frontend->session->ctx);
}

static bool frontend_has_non_routable_grab(TypioWlFrontend *frontend,
                                           bool now_active) {
    if (!frontend || !frontend->keyboard) {
        return false;
    }

    return !now_active || !session_is_focused(frontend) ||
           frontend->lifecycle_phase != TYPIO_WL_PHASE_ACTIVE;
}

static bool has_active_engine(TypioWlFrontend *frontend) {
    TypioRegistry *registry;
    char *name;
    bool has;

    if (!frontend || !frontend->instance) {
        return false;
    }

    registry = typio_instance_get_registry(frontend->instance);
    if (!registry) {
        return false;
    }

    name = typio_registry_get_active_keyboard(registry);
    has = name && *name;
    typio_free_string(name);
    return has;
}

static void trace_session_state(TypioWlFrontend *frontend, const char *event) {
    bool focused;
    bool pending_active;
    uint32_t serial;

    if (!frontend) {
        return;
    }

    focused = session_is_focused(frontend);
    pending_active = frontend->session ? frontend->session->pending.active : false;
    serial = frontend->im_serial;

    typio_wl_trace(frontend,
                   "im_state",
                   "event=%s phase=%s focused=%s pending_active=%s activate_seen=%s session=%s keyboard=%s serial=%u",
                   event ? event : "unknown",
                   typio_wl_lifecycle_phase_name(frontend->lifecycle_phase),
                   focused ? "yes" : "no",
                   pending_active ? "yes" : "no",
                   frontend->activate_seen ? "yes" : "no",
                   frontend->session ? "yes" : "no",
                   frontend->keyboard ? "yes" : "no",
                   serial);
}

static bool rebuild_keyboard_grab(TypioWlFrontend *frontend,
                                  const char *reset_reason,
                                  const char *failure_reason) {
    if (!frontend) {
        return false;
    }

    bool had_keyboard = frontend->keyboard != NULL;
    typio_wl_trace(frontend,
                   "keyboard_grab",
                   "action=rebuild begin reason=%s phase=%s focused=%s existing_keyboard=%s",
                   reset_reason ? reset_reason : "keyboard rebuild",
                   typio_wl_lifecycle_phase_name(frontend->lifecycle_phase),
                   session_is_focused(frontend) ? "yes" : "no",
                   had_keyboard ? "yes" : "no");
    typio_wl_lifecycle_hard_reset_keyboard(frontend,
                                           reset_reason ? reset_reason : "keyboard rebuild");
    frontend->keyboard = typio_wl_keyboard_create(frontend);
    if (!frontend->keyboard) {
        typio_log_error("%s", failure_reason ? failure_reason : "Failed to rebuild keyboard grab");
        typio_wl_trace_level(TYPIO_LOG_ERROR,
                             frontend,
                             "keyboard_grab",
                             "action=rebuild result=failed reason=%s phase=%s focused=%s",
                             failure_reason ? failure_reason : "Failed to rebuild keyboard grab",
                             typio_wl_lifecycle_phase_name(frontend->lifecycle_phase),
                             session_is_focused(frontend) ? "yes" : "no");
        return false;
    }

    typio_wl_vk_expect_keymap(frontend, "keyboard grab rebuilt");

    typio_wl_trace(frontend,
                   "keyboard_grab",
                   "action=rebuild result=ok created_at_epoch=%" PRIu64,
                   frontend->keyboard->created_at_epoch);
    return true;
}

/* Session management */
TypioWlSession *typio_wl_session_create(TypioWlFrontend *frontend) {
    TypioWlSession *session = calloc(1, sizeof(TypioWlSession));
    if (!session) {
        return nullptr;
    }

    session->frontend = frontend;

    /* Create input context */
    session->ctx = typio_instance_create_context(frontend->instance);
    if (!session->ctx) {
        typio_log_error("Failed to create input context");
        free(session);
        return nullptr;
    }

    /* Set up callbacks */
    typio_input_context_set_commit_callback(session->ctx, on_commit_callback, session);
    typio_input_context_set_composition_callback(session->ctx, on_composition_callback, session);
    typio_input_context_set_delete_surrounding_callback(session->ctx,
                                                        on_delete_surrounding_callback,
                                                        session);
    typio_input_context_set_user_data(session->ctx, session);

    return session;
}

void typio_wl_session_destroy(TypioWlSession *session) {
    if (!session) {
        return;
    }

    if (session->ctx) {
        typio_input_context_focus_out(session->ctx);
        typio_instance_destroy_context(session->frontend->instance, session->ctx);
    }

    free(session->last_preedit_text);
    free(session->pending.surrounding_text);
    free(session->current.surrounding_text);
    free(session);
}

void typio_wl_session_reset(TypioWlSession *session) {
    if (!session) {
        return;
    }

    /* Reset preedit change tracking and cancel any deferred panel work from
     * the previous activation so stale candidates cannot be redrawn later. */
    typio_wl_session_cancel_ui_tracking(session);

    /* Reset pending state */
    free(session->pending.surrounding_text);
    session->pending.surrounding_text = nullptr;
    session->pending.cursor = 0;
    session->pending.anchor = 0;
    session->pending.content_hint = 0;
    session->pending.content_purpose = 0;
    session->pending.text_change_cause = 0;
    session->pending.active = false;
}

void typio_wl_session_apply_pending(TypioWlSession *session) {
    if (!session) {
        return;
    }

    /* Apply surrounding text */
    free(session->current.surrounding_text);
    session->current.surrounding_text = session->pending.surrounding_text;
    session->current.cursor = session->pending.cursor;
    session->current.anchor = session->pending.anchor;
    session->pending.surrounding_text = nullptr;

    /* Apply content type */
    session->current.content_hint = session->pending.content_hint;
    session->current.content_purpose = session->pending.content_purpose;

    /* Update context with surrounding text if available */
    if (session->current.surrounding_text && session->ctx) {
        typio_input_context_set_surrounding(session->ctx,
                                            session->current.surrounding_text,
                                            (int)session->current.cursor,
                                            (int)session->current.anchor);
    }
}

/* Commit helpers */
void typio_wl_commit_string(TypioWlFrontend *frontend, const char *text) {
    if (!frontend || !frontend->input_method || !text) {
        return;
    }
    zwp_input_method_v2_commit_string(frontend->input_method, text);
}

void typio_wl_delete_surrounding(TypioWlFrontend *frontend,
                                 uint32_t before, uint32_t after) {
    if (!frontend || !frontend->input_method || (before == 0 && after == 0)) {
        return;
    }
    zwp_input_method_v2_delete_surrounding_text(frontend->input_method,
                                                before, after);
}

void typio_wl_set_preedit(TypioWlFrontend *frontend, const char *text,
                          int cursor_begin, int cursor_end) {
    if (!frontend || !frontend->input_method) {
        return;
    }
    zwp_input_method_v2_set_preedit_string(frontend->input_method,
                                           text ? text : "",
                                           cursor_begin, cursor_end);
}

void typio_wl_commit(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->input_method || !frontend->session) {
        return;
    }

    /* The zwp_input_method_v2 commit serial is the count of `done` events
     * received. A serial of 0 means the compositor has not yet sent a
     * single done — the input method is not established, and any
     * preedit/commit_string we staged would be silently dropped by the
     * compositor. Skip the commit and keep the staged state pending until
     * the first done arrives. (This is the single chokepoint for every
     * commit in the frontend, so future reconnect/multi-source work has
     * one place to revalidate the serial.) */
    if (frontend->im_serial == 0) {
        typio_wl_trace(frontend,
                       "commit",
                       "action=skip reason=no_done_yet serial=0 phase=%s",
                       typio_wl_lifecycle_phase_name(frontend->lifecycle_phase));
        return;
    }

    zwp_input_method_v2_commit(frontend->input_method, frontend->im_serial);
    frontend->last_committed_serial = frontend->im_serial;
}

/* Input method event handlers */
static void im_handle_activate(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im) {
    TypioWlFrontend *frontend = data;

    typio_wl_trace(frontend, "im", "event=activate");
    trace_session_state(frontend, "activate_begin");

    /* Create session if needed */
    if (!frontend->session) {
        frontend->session = typio_wl_session_create(frontend);
        if (!frontend->session) {
            typio_log_error("Failed to create session on activate");
            return;
        }
    }

    /* A (re)activation supersedes any positioned indicator from the prior
     * activation. Hide it now so it never lingers — or worse, gets repositioned
     * by the compositor onto the new text field's caret. Only the INDICATOR
     * owner is affected; a live candidate panel is left untouched. The matching
     * re-reveal happens in transition_to_active / transition_to_reactivate. */
    typio_wl_frontend_hide_indicator(frontend);

    /* Record that an activate arrived in this batch. The next `done` consumes
     * this to tell a genuine (re)activation apart from a plain text-state
     * update done (which must not rebuild focus state mid-composition). */
    frontend->activate_seen = true;
    typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_ACTIVATING,
                                 "activate event");

    /* Reset session state for new activation */
    typio_wl_session_reset(frontend->session);
    frontend->session->pending.active = true;
    trace_session_state(frontend, "activate_end");
}

static void im_handle_deactivate(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im) {
    TypioWlFrontend *frontend = data;

    typio_wl_trace(frontend, "im", "event=deactivate");
    trace_session_state(frontend, "deactivate_begin");
    typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_DEACTIVATING, "deactivate event");

    if (frontend->session) {
        frontend->session->pending.active = false;
    }
    trace_session_state(frontend, "deactivate_end");
}

static void im_handle_surrounding_text(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im,
                                       const char *text, uint32_t cursor,
                                       uint32_t anchor) {
    TypioWlFrontend *frontend = data;

    typio_wl_trace(frontend,
                   "im",
                   "event=surrounding_text cursor=%u anchor=%u has_text=%s",
                   cursor, anchor, text ? "yes" : "no");

    if (!frontend->session) {
        return;
    }

    free(frontend->session->pending.surrounding_text);
    frontend->session->pending.surrounding_text = text ? typio_strdup(text) : nullptr;
    frontend->session->pending.cursor = cursor;
    frontend->session->pending.anchor = anchor;
}

static void im_handle_text_change_cause(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im,
                                        uint32_t cause) {
    TypioWlFrontend *frontend = data;

    typio_wl_trace(frontend, "im", "event=text_change_cause cause=%u", cause);

    if (frontend->session) {
        frontend->session->pending.text_change_cause = cause;
    }
}

static void im_handle_content_type(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im,
                                   uint32_t hint, uint32_t purpose) {
    TypioWlFrontend *frontend = data;

    typio_wl_trace(frontend,
                   "im",
                   "event=content_type hint=0x%x purpose=%u",
                   hint, purpose);

    if (frontend->session) {
        frontend->session->pending.content_hint = hint;
        frontend->session->pending.content_purpose = purpose;
    }
}

static void transition_to_active(TypioWlFrontend *frontend) {
    typio_log_info("Input context focused");
    typio_input_context_focus_in(frontend->session->ctx);
    typio_wl_frontend_refresh_identity(frontend);
    typio_wl_frontend_restore_identity_engine(frontend);

    if (!has_active_engine(frontend)) {
        typio_log_warning("No active engine, skipping keyboard grab");
        typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_ACTIVE,
                                     "focus in without active engine");
        return;
    }

    if (!rebuild_keyboard_grab(frontend,
                               "focus in before new grab",
                               "Failed to create keyboard grab on activation")) {
        typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_INACTIVE,
                                     "focus in keyboard create failed");
        return;
    }

    typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_ACTIVE, "focus in complete");
    typio_wl_panel_coordinator_reset_anchor(frontend);

    {
        const TypioKeyboardEngineMode *mode =
            typio_instance_get_last_keyboard_mode(frontend->instance);
        if (mode && mode->display_label && mode->display_label[0]) {
            typio_wl_frontend_show_indicator_on_focus(frontend, mode);
        }
    }

    trace_session_state(frontend, "done_focus_in_complete");
}

static void transition_to_reactivate(TypioWlFrontend *frontend) {
    /* Re-activated while staying active: the compositor moved us to a
     * (possibly different) text field without an intervening deactivate. The
     * keyboard grab and the engine's input context persist for the same
     * input-method, so they are left intact — we only settle the phase back to
     * ACTIVE and re-anchor to the new caret. The indicator is not re-shown:
     * the engine state has not changed across a reactivation, so the old
     * indicator (already hidden by im_handle_activate) should not carry over.
     * Any deliberate mode/engine change during reactivation is announced via
     * show_indicator_for_state, not this path. */
    typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_ACTIVE, "reactivate complete");
    typio_wl_panel_coordinator_reset_anchor(frontend);

    trace_session_state(frontend, "done_reactivate_complete");
}

static void transition_to_inactive(TypioWlFrontend *frontend, const char *reason) {
    typio_log_info("Input context unfocused");
    typio_wl_panel_coordinator_reset_anchor(frontend);
    typio_wl_panel_coordinator_clear_caret_rect(frontend);
    typio_wl_session_cancel_ui_tracking(frontend->session);
    typio_input_context_focus_out(frontend->session->ctx);
    typio_input_context_reset(frontend->session->ctx);
    typio_wl_panel_coordinator_hide_all(frontend);

    typio_wl_lifecycle_hard_reset_keyboard(frontend, reason);
    typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_INACTIVE, "focus out complete");
    typio_wl_frontend_clear_identity(frontend);
    trace_session_state(frontend, "done_focus_out_complete");
}

void typio_wl_input_method_handle_resume(TypioWlFrontend *frontend,
                                         const char *reason,
                                         uint64_t sleep_ms) {
    bool was_active;

    if (!frontend) {
        return;
    }

    /* Capture intent before the scrub clears it: were we actively
     * composing in a focused client when the machine went to sleep? */
    was_active = session_is_focused(frontend) &&
                 frontend->lifecycle_phase == TYPIO_WL_PHASE_ACTIVE;

    /* Pure scrub: drop the grab, stale modifiers, key tracking, and the
     * compositor preedit; force the phase to INACTIVE. */
    typio_wl_lifecycle_on_resume(frontend, reason, sleep_ms);

    if (!was_active) {
        /* Not composing at suspend time. The next compositor activate
         * runs the full path; nothing more to do here. */
        return;
    }

    /* We *were* composing. Some compositors redeliver the full IM
     * handshake on wake, but many keep their view of focus unchanged and
     * send nothing — which would leave us deaf with no grab. Rebuild the
     * grab proactively. The input context was never focus_out'd, so the
     * engine's in-flight composition survives. */
    if (!has_active_engine(frontend)) {
        typio_log_warning("Resume: no active engine, leaving input method inactive");
        return;
    }

    typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_ACTIVATING,
                                 "resume regrab");
    if (!rebuild_keyboard_grab(frontend,
                               "resume regrab",
                               "Failed to recreate keyboard grab on resume")) {
        typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_INACTIVE,
                                     "resume regrab failed");
        return;
    }
    typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_ACTIVE,
                                 "resume regrab complete");
    trace_session_state(frontend, "resume_regrab_complete");
}

static void im_handle_done(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im) {
    TypioWlFrontend *frontend = data;

    frontend->im_serial++;

    if (!frontend->session) {
        typio_log_warning("Received done event without session (serial=%u)",
                  frontend->im_serial);
        frontend->activate_seen = false;
        return;
    }

    trace_session_state(frontend, "done_begin");
    bool was_active = session_is_focused(frontend);
    bool now_active = frontend->session->pending.active;
    bool activate_seen = frontend->activate_seen;
    frontend->activate_seen = false;

    TypioWlDoneAction action =
        typio_wl_lifecycle_classify_done(was_active, now_active, activate_seen);

    typio_wl_trace(frontend,
                   "im_done",
                   "was_active=%s now_active=%s activate_seen=%s action=%d phase=%s",
                   was_active ? "yes" : "no",
                   now_active ? "yes" : "no",
                   activate_seen ? "yes" : "no",
                   (int)action,
                   typio_wl_lifecycle_phase_name(frontend->lifecycle_phase));

    /* Apply pending state */
    typio_wl_session_apply_pending(frontend->session);

    switch (action) {
    case TYPIO_WL_DONE_ACTIVATE:
        transition_to_active(frontend);
        break;
    case TYPIO_WL_DONE_REACTIVATE:
        transition_to_reactivate(frontend);
        break;
    case TYPIO_WL_DONE_DEACTIVATE:
        transition_to_inactive(frontend, "focus out");
        break;
    case TYPIO_WL_DONE_NONE:
        /* A `done` with no state change — typically a text-state update. Guard
         * against a keyboard grab that is somehow still active in a state that
         * cannot route keys, and recover it. */
        if (frontend_has_non_routable_grab(frontend, now_active)) {
            typio_log_warning("Done without state transition but keyboard grab is non-routable: phase=%s was_active=%s now_active=%s focused=%s",
                      typio_wl_lifecycle_phase_name(frontend->lifecycle_phase),
                      was_active ? "yes" : "no",
                      now_active ? "yes" : "no",
                      session_is_focused(frontend) ? "yes" : "no");
            if (!now_active || !session_is_focused(frontend)) {
                typio_log_warning("Recovering from stale non-routable keyboard grab after done without transition");
                typio_wl_lifecycle_hard_reset_keyboard(
                    frontend, "done no transition stale grab");
                typio_wl_lifecycle_set_phase(frontend, TYPIO_WL_PHASE_INACTIVE,
                                             "done no transition stale grab");
            }
        }
        trace_session_state(frontend, "done_no_transition");
        break;
    }
}

static void im_handle_unavailable(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im) {
    TypioWlFrontend *frontend = data;

    typio_log_warning("Input method unavailable - another IM may be active");

    /* Stop the frontend */
    frontend->running = false;
    snprintf(frontend->error_msg, sizeof(frontend->error_msg),
             "Input method unavailable - another input method may be active");
}

/* Typio callbacks */
static void on_commit_callback([[maybe_unused]] TypioInputContext *ctx, const char *text,
                               void *user_data) {
    TypioWlSession *session = user_data;

    if (!session || !text || !text[0]) {
        return;
    }

    typio_log_debug("Commit: %s", text);

    /* Clear preedit first */
    typio_wl_set_preedit(session->frontend, "", -1, -1);
    typio_wl_panel_coordinator_hide(session->frontend, TYPIO_WL_UI_OWNER_CANDIDATE);

    /* The commit ends the candidate session. libtypio cleared its own
     * composition silently, so reset the host-side candidate-guard state here;
     * otherwise a later Left/Right would still be consumed as stale candidate
     * navigation. */
    session_clear_candidate_state(session);

    /* Commit the text */
    typio_wl_commit_string(session->frontend, text);

    /* Apply changes */
    typio_wl_commit(session->frontend);

    typio_wl_session_cancel_ui_tracking(session);

    /* Notify the registry that the active engine committed text,
     * so the recent-engine pair used for slow-switch toggling stays current. */
    typio_registry_notify_keyboard_commit(
        typio_instance_get_registry(session->frontend->instance));
}

static void on_delete_surrounding_callback([[maybe_unused]] TypioInputContext *ctx,
                                           uint32_t before, uint32_t after,
                                           void *user_data) {
    TypioWlSession *session = user_data;

    if (!session || !session->frontend || (before == 0 && after == 0)) {
        return;
    }

    typio_log_debug("Delete surrounding: before=%u after=%u", before, after);

    /* delete_surrounding_text + commit, mirroring the commit-string path: the
     * deletion is staged on the input-method object and applied by commit(). */
    typio_wl_delete_surrounding(session->frontend, before, after);
    typio_wl_commit(session->frontend);
}

static void on_composition_callback([[maybe_unused]] TypioInputContext *ctx,
                                    const TypioComposition *composition,
                                    void *user_data) {
    TypioWlSession *session = (TypioWlSession *)user_data;

    if (!session || !session->frontend) {
        return;
    }

    if (composition) {
        session->last_candidate_count = composition->candidate_count;
        session->last_candidate_selected = composition->selected;
        session->last_host_managed_selection = composition->host_managed_selection;
        candidate_snapshot_assign(&session->candidate_snapshot, composition);
    } else {
        session_clear_candidate_state(session);
    }

    typio_wl_session_request_ui_update(session);
}

void typio_wl_session_request_ui_update(TypioWlSession *session) {
    if (!session || !session->frontend) {
        return;
    }

    session->frontend->panel_schedule_state =
        typio_wl_panel_scheduler_mark_dirty(
            session->frontend->panel_schedule_state);
}

void typio_wl_session_cancel_ui_tracking(TypioWlSession *session) {
    if (!session) {
        return;
    }

    if (session->frontend) {
        session->frontend->panel_schedule_state =
            typio_wl_panel_scheduler_cancel();
    }
    typio_wl_text_ui_reset_tracking(&session->last_preedit_text,
                                    &session->last_preedit_cursor);
}

void typio_wl_session_flush_scheduled_ui_update(TypioWlSession *session) {
    if (!session || !session->ctx) {
        return;
    }
    update_wayland_text_ui(session, session->ctx);
}

static void update_wayland_text_ui(TypioWlSession *session, TypioInputContext *ctx) {
    const TypioPreedit *preedit;
    char *plain_text;
    int cursor_pos = -1;
    TypioWlTextUiPlan update_plan;
    uint64_t start_ms;
    uint64_t panel_done_ms;
    uint64_t end_ms;
    uint64_t panel_ms;
    uint64_t total_ms;

    if (!session || !ctx) {
        return;
    }

    start_ms = typio_wl_monotonic_ms();
    preedit = typio_input_context_get_preedit(ctx);
    plain_text = typio_wl_build_plain_preedit(preedit, &cursor_pos);

    /* Detect whether the preedit actually changed compared to what we
     * last sent to the application.  When only the candidate highlight
     * moved (e.g. Up/Down navigation) the preedit stays identical and
     * we can skip the protocol commit, avoiding an expensive
     * composition-update round-trip in heavyweight clients like Chrome. */
    const char *new_text = plain_text ? plain_text : "";
    update_plan = typio_wl_text_ui_plan_update(session->last_preedit_text,
                                               session->last_preedit_cursor,
                                               new_text,
                                               cursor_pos);

    /* Scheduled Panel updates run from the event loop. When the preedit is
     * unchanged, skip the protocol round-trip to the focused application and
     * refresh only the Panel. */
    TypioPanelUpdateResult panel_result =
        typio_wl_panel_coordinator_show_candidates(session->frontend, ctx);
    panel_done_ms = typio_wl_monotonic_ms();

    session->frontend->panel_schedule_state =
        typio_wl_panel_scheduler_complete(panel_result);
    if (panel_result == TYPIO_PANEL_UPDATE_OK &&
               session->candidate_snapshot.count > 0) {
        typio_wl_panel_coordinator_mark_anchor_ready(session->frontend,
                                                     "candidate_present");
    }
    if (update_plan == TYPIO_WL_TEXT_UI_SYNC_PREEDIT_AND_PANEL) {
        if (!plain_text) {
            typio_wl_set_preedit(session->frontend, "", -1, -1);
        } else {
            typio_wl_set_preedit(session->frontend, plain_text, cursor_pos, cursor_pos);
        }
        typio_wl_commit(session->frontend);

    }

    free(session->last_preedit_text);
    session->last_preedit_text = plain_text ? typio_strdup(new_text) : nullptr;
    session->last_preedit_cursor = cursor_pos;

    end_ms = typio_wl_monotonic_ms();
    panel_ms = (panel_done_ms >= start_ms) ? (panel_done_ms - start_ms) : 0;
    total_ms = (end_ms >= start_ms) ? (end_ms - start_ms) : 0;
    if (total_ms >= TYPIO_WL_UI_SLOW_UPDATE_MS) {
        typio_log_debug(
            "Wayland text UI slow: total=%" PRIu64 "ms panel=%" PRIu64 "ms preedit_changed=%s",
            total_ms,
            panel_ms,
            update_plan == TYPIO_WL_TEXT_UI_SYNC_PREEDIT_AND_PANEL ? "yes" : "no");
    }

    free(plain_text);
}
