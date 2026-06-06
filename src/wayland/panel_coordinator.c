/**
 * @file panel_coordinator.c
 * @brief Frontend-side Panel ownership, positioning, and anchor probing.
 *
 * This file coordinates Panel producers. It does not render: rendering remains
 * in src/ui/panel/. The coordinator decides which producer owns the single
 * Panel surface, when positioned status UI may be shown, and when a no-op
 * input-method commit should probe for a fresh cursor anchor.
 */

#include "internal.h"
#include "clock.h"
#include "panel.h"
#include "trace.h"

#include "typio/abi/input_context.h"
#include "typio/abi/config.h"
#include "typio/runtime/instance.h"

#include <inttypes.h>
#include <stdio.h>

#define TYPIO_ANCHOR_PROBE_DEFAULT_ENABLED true
#define TYPIO_ANCHOR_PROBE_DEFAULT_TIMEOUT_MS 150

static const char *ui_owner_name(TypioWlUiOwner owner);

bool typio_wl_panel_coordinator_anchor_ready(const TypioWlFrontend *frontend) {
    return frontend && frontend->panel_coord &&
           frontend->panel_coord->position_anchor_generation > 0 &&
           frontend->panel_coord->position_anchor_ready_generation ==
               frontend->panel_coord->position_anchor_generation;
}

void typio_wl_panel_coordinator_reset_anchor(TypioWlFrontend *frontend) {
    if (!frontend) return;
    frontend->panel_coord->position_anchor_generation++;
    if (frontend->panel_coord->position_anchor_generation == 0) {
        frontend->panel_coord->position_anchor_generation = 1;
    }
    frontend->panel_coord->position_anchor_ready_generation = 0;
    frontend->panel_coord->position_anchor_probe_generation = 0;
    frontend->panel_coord->position_anchor_has_caret = false;
}

void typio_wl_panel_coordinator_note_caret_rect(TypioWlFrontend *frontend) {
    if (!frontend) return;
    frontend->panel_coord->position_anchor_has_caret = true;
}

void typio_wl_panel_coordinator_clear_caret_rect(TypioWlFrontend *frontend) {
    if (!frontend) return;
    frontend->panel_coord->position_anchor_has_caret = false;
}

void typio_wl_panel_coordinator_mark_anchor_ready(TypioWlFrontend *frontend,
                                                   const char *reason) {
    if (!frontend || !frontend->panel_coord || frontend->panel_coord->position_anchor_generation == 0) return;
    if (typio_wl_panel_coordinator_anchor_ready(frontend)) return;

    frontend->panel_coord->position_anchor_ready_generation =
        frontend->panel_coord->position_anchor_generation;
    typio_wl_trace(frontend,
                   "position",
                   "action=anchor_ready reason=%s generation=%" PRIu64,
                   reason ? reason : "unknown",
                   frontend->panel_coord->position_anchor_generation);
}

static bool anchor_probe_enabled(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->instance) {
        return TYPIO_ANCHOR_PROBE_DEFAULT_ENABLED;
    }

    TypioConfig *cfg = typio_instance_get_config(frontend->instance);
    if (!cfg) {
        return TYPIO_ANCHOR_PROBE_DEFAULT_ENABLED;
    }

    return typio_config_get_bool(cfg,
                                 "display.anchor_probe",
                                 TYPIO_ANCHOR_PROBE_DEFAULT_ENABLED);
}

int typio_wl_panel_coordinator_anchor_timeout_ms(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->instance) {
        return TYPIO_ANCHOR_PROBE_DEFAULT_TIMEOUT_MS;
    }

    TypioConfig *cfg = typio_instance_get_config(frontend->instance);
    if (!cfg) {
        return TYPIO_ANCHOR_PROBE_DEFAULT_TIMEOUT_MS;
    }

    int ms = typio_config_get_int(cfg,
                                  "display.anchor_probe_timeout_ms",
                                  TYPIO_ANCHOR_PROBE_DEFAULT_TIMEOUT_MS);
    if (ms < 50) ms = 50;
    if (ms > 1000) ms = 1000;
    return ms;
}

static void maybe_probe_position_anchor(TypioWlFrontend *frontend,
                                        TypioWlUiOwner owner) {
    if (!frontend || !frontend->input_method || !frontend->session) return;
    if (!anchor_probe_enabled(frontend)) return;
    if (frontend->panel_coord->position_anchor_generation == 0) return;
    if (frontend->panel_coord->position_anchor_probe_generation ==
        frontend->panel_coord->position_anchor_generation) {
        return;
    }

    frontend->panel_coord->position_anchor_probe_generation =
        frontend->panel_coord->position_anchor_generation;
    typio_wl_trace(frontend,
                   "ui",
                   "action=anchor_probe owner=%s generation=%" PRIu64,
                   ui_owner_name(owner),
                   frontend->panel_coord->position_anchor_generation);
    typio_wl_set_preedit(frontend, "", -1, -1);
    typio_wl_commit(frontend);
}

static const char *ui_owner_name(TypioWlUiOwner owner) {
    switch (owner) {
    case TYPIO_WL_UI_OWNER_CANDIDATE:
        return "candidate";
    case TYPIO_WL_UI_OWNER_INDICATOR:
        return "indicator";
    case TYPIO_WL_UI_OWNER_VOICE:
        return "voice";
    case TYPIO_WL_UI_OWNER_NONE:
    default:
        return "none";
    }
}

void typio_wl_panel_coordinator_cancel_pending(TypioWlFrontend *frontend) {
    if (!frontend) return;
    frontend->panel_coord->positioned_ui_pending = false;
    frontend->panel_coord->positioned_ui_pending_owner = TYPIO_WL_UI_OWNER_NONE;
    frontend->panel_coord->positioned_ui_pending_since_ms = 0;
    frontend->panel_coord->positioned_ui_pending_label[0] = '\0';
}

static bool has_focused_context(TypioWlFrontend *frontend) {
    return frontend &&
           frontend->session &&
           frontend->session->ctx &&
           typio_input_context_is_focused(frontend->session->ctx);
}

void typio_wl_panel_coordinator_early_anchor_probe(TypioWlFrontend *frontend) {
    if (!frontend) return;
    maybe_probe_position_anchor(frontend, TYPIO_WL_UI_OWNER_NONE);
}

static bool queue_positioned_ui(TypioWlFrontend *frontend,
                                TypioWlUiOwner owner,
                                const char *text) {
    if (!frontend || !text || !text[0] || !has_focused_context(frontend)) {
        return false;
    }

    if (!frontend->panel_coord->positioned_ui_pending ||
        frontend->panel_coord->positioned_ui_pending_owner != owner) {
        frontend->panel_coord->positioned_ui_pending_since_ms = typio_wl_monotonic_ms();
    }
    frontend->panel_coord->positioned_ui_pending_owner = owner;
    snprintf(frontend->panel_coord->positioned_ui_pending_label,
             sizeof(frontend->panel_coord->positioned_ui_pending_label),
             "%s",
             text);
    frontend->panel_coord->positioned_ui_pending = true;
    typio_wl_trace(frontend,
                   "ui",
                   "action=queue owner=%s reason=position_anchor_not_ready generation=%" PRIu64,
                   ui_owner_name(owner),
                   frontend->panel_coord->position_anchor_generation);
    maybe_probe_position_anchor(frontend, owner);
    return true;
}

TypioPanelUpdateResult typio_wl_panel_coordinator_show_candidates(TypioWlFrontend *frontend,
                                                                  TypioInputContext *ctx) {
    if (!frontend || !frontend->panel) return TYPIO_PANEL_UPDATE_FAIL;

    size_t candidate_count = 0;
    if (frontend->session) {
        candidate_count = frontend->session->candidate_snapshot.count;
    }

    if (candidate_count == 0) {
        typio_wl_panel_coordinator_hide(frontend, TYPIO_WL_UI_OWNER_CANDIDATE);
        return TYPIO_PANEL_UPDATE_OK;
    }

    typio_wl_panel_coordinator_cancel_pending(frontend);
    if (frontend->ui_owner != TYPIO_WL_UI_OWNER_CANDIDATE) {
        typio_panel_hide_status(frontend->panel);
    }

    frontend->ui_owner = TYPIO_WL_UI_OWNER_CANDIDATE;
    return typio_panel_update(frontend->panel, ctx);
}

bool typio_wl_panel_coordinator_show_status(TypioWlFrontend *frontend,
                                             TypioWlUiOwner owner,
                                             const char *text) {
    if (!frontend || !frontend->panel || !text || !text[0]) return false;
    if (owner == TYPIO_WL_UI_OWNER_NONE ||
        owner == TYPIO_WL_UI_OWNER_CANDIDATE) {
        return false;
    }

    if (!typio_wl_panel_coordinator_anchor_ready(frontend)) {
        return queue_positioned_ui(frontend, owner, text);
    }

    typio_wl_panel_coordinator_cancel_pending(frontend);
    if (frontend->ui_owner != owner) {
        typio_panel_hide(frontend->panel);
    }

    bool ok = typio_panel_show_status(frontend->panel, text);
    if (ok) {
        frontend->ui_owner = owner;
    }
    return ok;
}

void typio_wl_panel_coordinator_hide(TypioWlFrontend *frontend,
                                      TypioWlUiOwner owner) {
    if (!frontend || !frontend->panel) return;

    if (frontend->panel_coord->positioned_ui_pending &&
        frontend->panel_coord->positioned_ui_pending_owner == owner) {
        typio_wl_panel_coordinator_cancel_pending(frontend);
    }

    if (frontend->ui_owner != owner) {
        return;
    }

    if (owner == TYPIO_WL_UI_OWNER_CANDIDATE) {
        typio_panel_hide(frontend->panel);
    } else {
        typio_panel_hide_status(frontend->panel);
    }
    frontend->ui_owner = TYPIO_WL_UI_OWNER_NONE;
}

void typio_wl_panel_coordinator_hide_all(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->panel) return;

    typio_wl_panel_coordinator_cancel_pending(frontend);
    typio_panel_hide_status(frontend->panel);
    typio_panel_hide(frontend->panel);
    frontend->ui_owner = TYPIO_WL_UI_OWNER_NONE;
}

void typio_wl_panel_coordinator_flush_pending(TypioWlFrontend *frontend) {
    TypioWlUiOwner owner;
    char label[TYPIO_POSITIONED_UI_LABEL_CAP];

    if (!frontend || !frontend->panel_coord || !frontend->panel_coord->positioned_ui_pending) return;
    if (!typio_wl_panel_coordinator_anchor_ready(frontend)) return;

    owner = frontend->panel_coord->positioned_ui_pending_owner;
    snprintf(label, sizeof(label), "%s", frontend->panel_coord->positioned_ui_pending_label);
    typio_wl_panel_coordinator_cancel_pending(frontend);
    typio_wl_panel_coordinator_show_status(frontend, owner, label);
}
