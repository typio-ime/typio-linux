#include "fallback_cache.h"

#include <limits.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    FcCharSet *coverage;   /* owned copy of the resolved font's charset */
    int32_t    weight;
    char      *path;       /* owned */
    uint32_t   lru_tick;
} FallbackEntry;

struct FallbackFontCache {
    FallbackEntry    *entries;
    size_t            cap;
    size_t            count;
    uint32_t          tick;
    FallbackResolveFn resolve;
    void             *user;
};

FallbackFontCache *fallback_cache_new(size_t cap, FallbackResolveFn resolve,
                                      void *user)
{
    if (cap == 0 || !resolve) return NULL;

    FallbackFontCache *c = calloc(1, sizeof(*c));
    if (!c) return NULL;

    c->entries = calloc(cap, sizeof(*c->entries));
    if (!c->entries) {
        free(c);
        return NULL;
    }
    c->cap     = cap;
    c->resolve = resolve;
    c->user    = user;
    return c;
}

void fallback_cache_free(FallbackFontCache *c)
{
    if (!c) return;
    for (size_t i = 0; i < c->count; ++i) {
        if (c->entries[i].coverage) FcCharSetDestroy(c->entries[i].coverage);
        free(c->entries[i].path);
    }
    free(c->entries);
    free(c);
}

/* Charset of every codepoint in @text (caller destroys). */
static FcCharSet *text_charset(const char *text)
{
    FcCharSet *cs = FcCharSetCreate();
    if (!cs) return NULL;
    const char *p = text;
    while (*p) {
        FcChar32 ch;
        int len = FcUtf8ToUcs4((const FcChar8 *)p, &ch, (int)strlen(p));
        if (len <= 0) break;
        FcCharSetAddChar(cs, ch);
        p += len;
    }
    return cs;
}

static void cache_insert(FallbackFontCache *c, FcCharSet *coverage,
                         int32_t weight, const char *path)
{
    FallbackEntry *slot;
    if (c->count < c->cap) {
        slot = &c->entries[c->count++];
    } else {
        /* Evict the least-recently-used entry. */
        size_t   lru = 0;
        uint32_t oldest = UINT32_MAX;
        for (size_t i = 0; i < c->cap; ++i) {
            if (c->entries[i].lru_tick < oldest) {
                oldest = c->entries[i].lru_tick;
                lru = i;
            }
        }
        slot = &c->entries[lru];
        if (slot->coverage) FcCharSetDestroy(slot->coverage);
        free(slot->path);
    }
    slot->coverage = coverage;
    slot->weight   = weight;
    slot->path     = path ? strdup(path) : NULL;
    slot->lru_tick = c->tick;
}

char *fallback_cache_lookup(FallbackFontCache *c, const char *text,
                            int32_t weight)
{
    if (!c || !text || !text[0]) return NULL;

    FcCharSet *want = text_charset(text);
    if (!want) {
        FcCharSet *cov = NULL;
        char *r = c->resolve(c->user, text, weight, &cov);
        if (cov) FcCharSetDestroy(cov);
        return r;
    }

    c->tick++;

    /* Hit: reuse a resolved fallback whose coverage is a superset of @text. */
    for (size_t i = 0; i < c->count; ++i) {
        FallbackEntry *e = &c->entries[i];
        if (e->weight == weight && e->coverage &&
            FcCharSetIsSubset(want, e->coverage)) {
            e->lru_tick = c->tick;
            char *r = e->path ? strdup(e->path) : NULL;
            FcCharSetDestroy(want);
            return r;
        }
    }
    FcCharSetDestroy(want);

    /* Miss: pay for the resolve once and remember its coverage. */
    FcCharSet *cov = NULL;
    char *result = c->resolve(c->user, text, weight, &cov);
    if (result && cov) {
        cache_insert(c, cov, weight, result);  /* takes ownership of cov */
    } else if (cov) {
        FcCharSetDestroy(cov);
    }
    return result;
}

size_t fallback_cache_entry_count(const FallbackFontCache *c)
{
    return c ? c->count : 0;
}
