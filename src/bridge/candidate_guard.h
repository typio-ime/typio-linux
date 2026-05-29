/**
 * @file candidate_guard.h
 * @brief Helpers for reserving navigation keys while candidate UI is active
 */

#ifndef TYPIO_WL_CANDIDATE_GUARD_H
#define TYPIO_WL_CANDIDATE_GUARD_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

struct TypioWlSession;

bool typio_wl_candidate_guard_is_navigation_keysym(uint32_t keysym);
bool typio_wl_candidate_guard_should_consume(struct TypioWlSession *session,
                                             uint32_t keysym);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_CANDIDATE_GUARD_H */
