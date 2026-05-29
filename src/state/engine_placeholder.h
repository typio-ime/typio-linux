/**
 * @file engine_placeholder.h
 * @brief Placeholder engine info for when no engine is loaded.
 */

#ifndef TYPIO_ENGINE_PLACEHOLDER_H
#define TYPIO_ENGINE_PLACEHOLDER_H

#include "typio/abi/engine.h"
#include "typio/runtime/registry.h"

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Return a static placeholder engine info used when no engine is active.
 *
 * The returned pointer is static and must NOT be passed to
 * typio_engine_info_free.  Use typio_engine_info_free_if_real()
 * to safely release a possibly-placeholder pointer.
 */
const TypioEngineInfo *typio_engine_info_placeholder(void);

/**
 * @brief Free an engine info pointer only if it is not the placeholder.
 */
static inline void typio_engine_info_free_if_real(const TypioEngineInfo *info)
{
    if (info && info != typio_engine_info_placeholder()) {
        typio_engine_info_free((TypioEngineInfo *)info);
    }
}

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_ENGINE_PLACEHOLDER_H */
