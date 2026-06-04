/**
 * @file input_method.c
 * @brief zwp_input_method_v2 event handlers — record facts and apply them
 *        at done time.
 *
 * The event handlers in this file do exactly one thing per protocol event:
 * record a fact into `frontend->session_facts` (or the session's `pending`
 * buffer for the engine state). The session controller consumes the facts
 * at the end of the event-loop tick, derives the desired resource
 * configuration, observes the live state, and applies the minimal
 * idempotent effect set. See `docs/explanation/session-controller.md`.
 *
 * The serial chokepoint lives in `typio_wl_commit()`: a commit before the
 * first `done` is silently dropped to keep the compositor from receiving
 * preedit text without an established input-method connection.
 */

#include "internal.h"
#include "panel.h"
#include "engine/niri/identity.h"
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
                       "action=skip reason=no_done_yet serial=0");
        return;
    }

    zwp_input_method_v2_commit(frontend->input_method, frontend->im_serial);
}

/* Input method event handlers
 *
 * Each handler records a fact. The session controller's per-tick pipeline
 * (event_loop.c) reads the facts and applies the right effects. There are
 * no imperative transitions here. */
static void im_handle_activate(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im) {
    TypioWlFrontend *frontend = data;

    typio_wl_trace(frontend, "im", "event=activate");

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
    frontend->session_facts.im_activate_seen = true;

    /* Reset session state for new activation. session_reset() clears
     * pending.active; the activate fact recorded above drives the
     * controller to want grab=YES at the next done. */
    typio_wl_session_reset(frontend->session);
    frontend->session->pending.active = true;
}

static void im_handle_deactivate(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im) {
    TypioWlFrontend *frontend = data;

    typio_wl_trace(frontend, "im", "event=deactivate");
    frontend->session_facts.im_deactivate_seen = true;

    if (frontend->session) {
        frontend->session->pending.active = false;
    }
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

static void im_handle_done(void *data, [[maybe_unused]] struct zwp_input_method_v2 *im) {
    TypioWlFrontend *frontend = data;

    frontend->im_serial++;

    if (!frontend->session) {
        typio_log_warning("Received done event without session (serial=%u)",
                  frontend->im_serial);
        return;
    }

    /* The done batch is the atomic commit point: it consumes the activate /
     * deactivate facts accumulated since the previous done and records a
     * single boolean for the controller to reduce. */
    bool was_active = typio_input_context_is_focused(frontend->session->ctx);
    frontend->session_facts.im_done_had_activate =
        frontend->session_facts.im_activate_seen;
    frontend->session_facts.im_done_serial = frontend->im_serial;

    /* Apply pending engine-context state (surrounding text, content type)
     * atomically. The activate_seen / im_done_had_activate / pending.active
     * facts are consumed by the session controller on the next pipeline run. */
    typio_wl_session_apply_pending(frontend->session);

    /* Clear the per-event activate / deactivate facts; the per-batch
     * im_done_had_activate flag survives until reduce() consumes it. */
    frontend->session_facts.im_activate_seen = false;
    frontend->session_facts.im_deactivate_seen = false;

    typio_wl_trace(frontend,
                   "im_done",
                   "was_active=%s now_active=%s serial=%u",
                   was_active ? "yes" : "no",
                   frontend->session->pending.active ? "yes" : "no",
                   frontend->im_serial);
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

    if (session->frontend->panel_coord) {
        session->frontend->panel_coord->panel_schedule_state =
            typio_wl_panel_scheduler_mark_dirty(
                session->frontend->panel_coord->panel_schedule_state);
    }
}

void typio_wl_session_cancel_ui_tracking(TypioWlSession *session) {
    if (!session) {
        return;
    }

    if (session->frontend && session->frontend->panel_coord) {
        session->frontend->panel_coord->panel_schedule_state =
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

    if (session->frontend->panel_coord) {
        session->frontend->panel_coord->panel_schedule_state =
            typio_wl_panel_scheduler_complete(panel_result);
    }
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
