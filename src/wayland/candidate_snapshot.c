/**
 * @file candidate_snapshot.c
 * @brief Heap-owned candidate-list deep copy + clear (see header).
 *
 * Extracted from input_method.c so the free path can be unit-tested without
 * linking the Wayland protocol surface. The semantics are unchanged from the
 * previous static helpers; only the location and visibility changed.
 */
#include "candidate_snapshot.h"
#include "internal.h"

#include "typio/abi/input_context.h"
#include "typio/abi/string.h"

#include <stdlib.h>
#include <string.h>

/* Allocation helper mirrored from input_method.c — returns a heap duplicate
 * of @s, or NULL when @s is NULL (so callers can pass possibly-NULL source
 * strings without an extra branch). */
static char *typio_dup_or_null(const char *s)
{
    return s ? typio_strdup(s) : NULL;
}

void typio_candidate_snapshot_clear(TypioCandidateList *snap)
{
    if (!snap) return;
    for (size_t i = 0; i < snap->count; i++) {
        free((char *)snap->candidates[i].text);
        free((char *)snap->candidates[i].comment);
        free((char *)snap->candidates[i].label);
    }
    free(snap->candidates);
    snap->candidates = NULL;
    snap->count = 0;
    snap->selected = -1;
    snap->total = 0;
    snap->page = 0;
    snap->page_size = 0;
    snap->has_prev = false;
    snap->has_next = false;
    snap->content_signature = 0;
}

bool typio_candidate_snapshot_equal_content(const TypioCandidateList *snap,
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

void typio_candidate_snapshot_assign(TypioCandidateList *snap,
                                      const TypioComposition *composition)
{
    if (!composition || composition->candidate_count == 0) {
        typio_candidate_snapshot_clear(snap);
        return;
    }

    /* Fast path: only the selected highlight moved.  Skip the expensive
     * clear + calloc + strdup round-trip.  This is the common case when
     * the user pages through RIME candidates with Up/Down. */
    if (typio_candidate_snapshot_equal_content(snap, composition)) {
        snap->selected = composition->selected;
        snap->content_signature = composition->content_signature;
        return;
    }

    typio_candidate_snapshot_clear(snap);
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

void typio_wl_session_clear_candidate_state(TypioWlSession *session)
{
    if (!session) return;
    session->last_candidate_count = 0;
    session->last_candidate_selected = -1;
    session->last_host_managed_selection = TYPIO_HOST_SEL_NONE;
    typio_candidate_snapshot_clear(&session->candidate_snapshot);
}
