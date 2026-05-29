/**
 * @file aux_adapters.h
 * @brief Convenience constructors that wrap Typio subsystems in TypioWlAuxHandler.
 */

#ifndef TYPIO_WL_AUX_ADAPTERS_H
#define TYPIO_WL_AUX_ADAPTERS_H

#include "aux_handler.h"
#include "resume.h"

/* Always available: resume detector has no build-time dependency beyond
 * libdbus (already required by the daemon) and degrades to the boottime
 * gap heuristic when the system bus is absent. */
TypioWlAuxHandler *typio_wl_aux_handler_for_resume_signal(TypioWlResumeSignal *rs);

/* IPC bus (UDS) — always available */
struct TypioIpcBus;
TypioWlAuxHandler *typio_wl_aux_handler_for_ipc_bus(struct TypioIpcBus *bus);

#ifdef HAVE_SYSTRAY
#include "tray/tray.h"
TypioWlAuxHandler *typio_wl_aux_handler_for_tray(TypioTray *tray);
#endif

#ifdef HAVE_VOICE
#include "typio/runtime/voice.h"
#include "typio/abi/voice.h"
TypioWlAuxHandler *typio_wl_aux_handler_for_voice(TypioVoiceSession *voice,
                                                    struct TypioWlFrontend *frontend);
#endif

#endif /* TYPIO_WL_AUX_ADAPTERS_H */
