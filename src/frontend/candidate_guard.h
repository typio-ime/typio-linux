/**
 * @file candidate_guard.h
 * @brief Host-managed candidate selection (libtypio ADR-0013)
 */

#ifndef TYPIO_WL_CANDIDATE_GUARD_H
#define TYPIO_WL_CANDIDATE_GUARD_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

struct TypioWlSession;
struct TypioWlFrontend;

typedef enum {
    TYPIO_WL_HOST_SEL_NONE = 0,
    TYPIO_WL_HOST_SEL_NAV_UP,
    TYPIO_WL_HOST_SEL_NAV_DOWN,
    TYPIO_WL_HOST_SEL_COMMIT_SELECTED,
    TYPIO_WL_HOST_SEL_COMMIT_RAW,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_1,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_2,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_3,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_4,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_5,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_6,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_7,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_8,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_9,
    TYPIO_WL_HOST_SEL_COMMIT_INDEX_0,
} TypioWlHostSelKey;

/* Classification of a keysym into a host-managed selection category. */
typedef enum {
    TYPIO_WL_HOST_SEL_CATEGORY_NONE,
    TYPIO_WL_HOST_SEL_CATEGORY_NAVIGATE,
    TYPIO_WL_HOST_SEL_CATEGORY_COMMIT,
    TYPIO_WL_HOST_SEL_CATEGORY_COMMIT_RAW,
    TYPIO_WL_HOST_SEL_CATEGORY_INDEX_PICK,
} TypioWlHostSelCategory;

bool typio_wl_candidate_guard_is_navigation_keysym(uint32_t keysym);
bool typio_wl_candidate_guard_should_consume(struct TypioWlSession *session,
                                             uint32_t keysym);

TypioWlHostSelKey typio_wl_host_selection_keysym(uint32_t keysym);
TypioWlHostSelCategory typio_wl_host_selection_category(TypioWlHostSelKey sel);
int typio_wl_host_selection_resolve(TypioWlHostSelKey sel,
                                    int current_selected,
                                    size_t candidate_count);
bool typio_wl_host_selection_is_commit(TypioWlHostSelKey sel);
bool typio_wl_host_selection_try_commit(struct TypioWlFrontend *frontend,
                                        struct TypioWlSession *session,
                                        TypioWlHostSelKey sel);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_CANDIDATE_GUARD_H */
