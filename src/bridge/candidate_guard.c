/**
 * @file candidate_guard.c
 * @brief Host-managed candidate selection (ADR-0012).
 *
 * When an engine publishes a composition with host-managed-selection flags set,
 * the host intercepts the corresponding navigation and selection keys before
 * they reach process_key. The host manages the selected index locally and
 * calls back via commit_candidate when the user makes a selection.
 */

#include "candidate_guard.h"
#include "internal.h"

#include <typio/abi/engine.h>
#include <typio/abi/input_context.h>
#include <xkbcommon/xkbcommon-keysyms.h>

bool typio_wl_candidate_guard_is_navigation_keysym(uint32_t keysym) {
    switch (keysym) {
    case XKB_KEY_Up:
    case XKB_KEY_Down:
    case XKB_KEY_Left:
    case XKB_KEY_Right:
        return true;
    default:
        return false;
    }
}

TypioWlHostSelCategory typio_wl_host_selection_category(TypioWlHostSelKey sel) {
    switch (sel) {
    case TYPIO_WL_HOST_SEL_NAV_UP:
    case TYPIO_WL_HOST_SEL_NAV_DOWN:
        return TYPIO_WL_HOST_SEL_CATEGORY_NAVIGATE;
    case TYPIO_WL_HOST_SEL_COMMIT_SELECTED:
        return TYPIO_WL_HOST_SEL_CATEGORY_COMMIT;
    case TYPIO_WL_HOST_SEL_COMMIT_RAW:
        return TYPIO_WL_HOST_SEL_CATEGORY_COMMIT_RAW;
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_1:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_2:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_3:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_4:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_5:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_6:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_7:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_8:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_9:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_0:
        return TYPIO_WL_HOST_SEL_CATEGORY_INDEX_PICK;
    default:
        return TYPIO_WL_HOST_SEL_CATEGORY_NONE;
    }
}

bool typio_wl_candidate_guard_should_consume(TypioWlSession *session,
                                             uint32_t keysym) {
    if (!session)
        return false;

    if (session->last_candidate_count == 0)
        return false;

    uint32_t flags = session->last_host_managed_selection;
    if (flags == 0)
        return typio_wl_candidate_guard_is_navigation_keysym(keysym);

    TypioWlHostSelKey sel = typio_wl_host_selection_keysym(keysym);
    if (sel == TYPIO_WL_HOST_SEL_NONE)
        return false;

    TypioWlHostSelCategory cat = typio_wl_host_selection_category(sel);
    switch (cat) {
    case TYPIO_WL_HOST_SEL_CATEGORY_NAVIGATE:
        return (flags & TYPIO_HOST_SEL_NAVIGATE) != 0;
    case TYPIO_WL_HOST_SEL_CATEGORY_COMMIT:
        return (flags & TYPIO_HOST_SEL_COMMIT) != 0;
    case TYPIO_WL_HOST_SEL_CATEGORY_COMMIT_RAW:
        return (flags & TYPIO_HOST_SEL_COMMIT_RAW) != 0;
    case TYPIO_WL_HOST_SEL_CATEGORY_INDEX_PICK:
        return (flags & TYPIO_HOST_SEL_INDEX_PICK) != 0;
    default:
        return false;
    }
}

TypioWlHostSelKey typio_wl_host_selection_keysym(uint32_t keysym) {
    switch (keysym) {
    case XKB_KEY_Up:
    case XKB_KEY_Left:
        return TYPIO_WL_HOST_SEL_NAV_UP;
    case XKB_KEY_Down:
    case XKB_KEY_Right:
        return TYPIO_WL_HOST_SEL_NAV_DOWN;
    case XKB_KEY_space:
        return TYPIO_WL_HOST_SEL_COMMIT_SELECTED;
    case XKB_KEY_Return:
    case XKB_KEY_KP_Enter:
        return TYPIO_WL_HOST_SEL_COMMIT_RAW;
    default:
        break;
    }

    if (keysym >= XKB_KEY_1 && keysym <= XKB_KEY_9)
        return TYPIO_WL_HOST_SEL_COMMIT_INDEX_1 + (keysym - XKB_KEY_1);

    if (keysym == XKB_KEY_0)
        return TYPIO_WL_HOST_SEL_COMMIT_INDEX_0;

    return TYPIO_WL_HOST_SEL_NONE;
}

int typio_wl_host_selection_resolve(TypioWlHostSelKey sel,
                                    int current_selected,
                                    size_t candidate_count) {
    if (candidate_count == 0)
        return -1;

    switch (sel) {
    case TYPIO_WL_HOST_SEL_NAV_UP:
        return current_selected > 0 ? current_selected - 1 : 0;
    case TYPIO_WL_HOST_SEL_NAV_DOWN: {
        int max = (int)candidate_count - 1;
        return current_selected < max ? current_selected + 1 : max;
    }
    case TYPIO_WL_HOST_SEL_COMMIT_SELECTED:
        return current_selected >= 0 ? current_selected : 0;
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_0: {
        int idx = 9;
        return idx < (int)candidate_count ? idx : -1;
    }
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_1:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_2:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_3:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_4:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_5:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_6:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_7:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_8:
    case TYPIO_WL_HOST_SEL_COMMIT_INDEX_9: {
        int idx = (int)(sel - TYPIO_WL_HOST_SEL_COMMIT_INDEX_1);
        return idx < (int)candidate_count ? idx : -1;
    }
    default:
        return -1;
    }
}

bool typio_wl_host_selection_is_commit(TypioWlHostSelKey sel) {
    return sel == TYPIO_WL_HOST_SEL_COMMIT_SELECTED ||
           sel == TYPIO_WL_HOST_SEL_COMMIT_RAW ||
           (sel >= TYPIO_WL_HOST_SEL_COMMIT_INDEX_1 &&
            sel <= TYPIO_WL_HOST_SEL_COMMIT_INDEX_9) ||
           sel == TYPIO_WL_HOST_SEL_COMMIT_INDEX_0;
}

bool typio_wl_host_selection_try_commit(TypioWlFrontend *frontend,
                                        TypioWlSession *session,
                                        TypioWlHostSelKey sel) {
    if (!frontend || !session || !session->ctx)
        return false;

    int idx = typio_wl_host_selection_resolve(sel,
                                              session->last_candidate_selected,
                                              session->last_candidate_count);
    if (idx < 0)
        return false;

    TypioResult result = typio_input_context_commit_candidate(session->ctx, idx);
    return result == TYPIO_OK;
}
