/**
 * @file candidate_snapshot.h
 * @brief Heap-owned deep copy of a TypioComposition's candidate list.
 *
 * libtypio delivers TypioComposition pointers only for the lifetime of the
 * composition callback; the panel renders later out of the event loop. The
 * Wayland session deep-copies the candidates it needs into a TypioCandidateList
 * (embedded by value in TypioWlSession) and frees them on the next callback,
 * on session teardown, and on focus-out teardown.
 *
 * The helpers live here — separate from input_method.c — so they can be
 * unit-tested without linking the Wayland protocol surface. The clear path
 * is the regression surface for two leak fixes:
 *
 *   - typio_wl_session_destroy previously freed the surrounding TypioWlSession
 *     struct but left the embedded TypioCandidateList's heap state (the
 *     candidates array + 3×N strings) dangling.
 *   - The discard_composition focus effect previously reset the engine + hid
 *     the panel but never cleared the snapshot, leaking on every focus-out /
 *     engine-switch.
 *
 * Both paths route through typio_wl_session_clear_candidate_state(); a unit
 * test exercises the clear directly to catch regressions in the free path.
 */
#ifndef TYPIO_WL_CANDIDATE_SNAPSHOT_H
#define TYPIO_WL_CANDIDATE_SNAPSHOT_H

#include "ui/panel/content.h"

#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

struct TypioWlSession;
struct TypioComposition;

/* Free the candidates array and every per-candidate heap string, then zero
 * the TypioCandidateList fields. Safe to call on an already-empty snapshot
 * (idempotent). The TypioCandidateList is embedded by value in TypioWlSession;
 * freeing its heap state without going through this helper leaks. */
void typio_candidate_snapshot_clear(TypioCandidateList *snap);

/* True iff @snap's content matches @comp's content byte-for-byte (count,
 * pagination flags, and every candidate's text/comment/label). Used by the
 * assign fast path to skip an unnecessary teardown + deep-copy round-trip
 * when only the selection highlight moved (the common case when paging). */
bool typio_candidate_snapshot_equal_content(const TypioCandidateList *snap,
                                             const struct TypioComposition *comp);

/* Deep-copy @comp's candidate list into @snap, freeing any previous snapshot
 * state first. No-op (clears only) when @comp is NULL or has zero candidates.
 * On allocation failure @snap is left empty (no partial state). */
void typio_candidate_snapshot_assign(TypioCandidateList *snap,
                                      const struct TypioComposition *comp);

/* Reset all candidate-session state derived from a composition: the
 * candidate-guard scalars consulted on every key and the heap-owned snapshot
 * used to re-render the panel. Called from:
 *
 *   - on_commit_callback     (text committed, libtypio silently cleared its
 *                             composition so we mirror that here)
 *   - on_composition_callback (NULL composition — engine reset)
 *   - discard_composition     (focus-out / engine-switch teardown)
 *   - typio_wl_session_destroy (final cleanup)
 *
 * Idempotent. */
void typio_wl_session_clear_candidate_state(struct TypioWlSession *session);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_CANDIDATE_SNAPSHOT_H */
