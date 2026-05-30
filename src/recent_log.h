#ifndef TYPIO_RECENT_LOG_H
#define TYPIO_RECENT_LOG_H

#ifdef __cplusplus
extern "C" {
#endif

/* Best-effort dump of the in-process recent-log ring buffer to the
 * per-app path configured at startup. No-op if no app is running or no
 * path was configured. Implemented in app.c. */
void typio_dump_recent_log(void);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_RECENT_LOG_H */
