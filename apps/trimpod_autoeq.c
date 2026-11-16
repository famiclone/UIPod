/***************************************************************************
 * Trimpod: AutoEQ headphone database search, ParametricEQ downloader/converter,
 * and local EQ preset browser (EQS_DIR).
 *
 * Searches the bundled (and Wi-Fi updatable) jaakkopasanen/AutoEq INDEX.md,
 * fetches `<Model> ParametricEQ.txt` over HTTPS, converts its Preamp + 10
 * biquad filters (1 LSC + 8 PK + 1 HSC) into Rockbox's 10-band Parametric EQ
 * `.cfg` format under EQS_DIR (/tmp/trimpod/eqs -> $PAK_DIR/trimpod/eqs), and
 * optionally applies the preset immediately.
 ****************************************************************************/
#include "config.h"
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include "string-extra.h"
#include "strnatcmp.h"
#include "file.h"
#include "dir.h"
#include "pathfuncs.h"
#include "rbpaths.h"
#include "settings.h"
#include "lang.h"
#include "splash.h"
#include "action.h"
#include "gui/list.h"
#include "icon.h"
#include "system.h"
#include "menus/eq_menu.h"
#include "https-target.h"
#include "trimpod_page.h"
#include "trimpod_transition.h"
#include "trimpod_keyboard.h"
#include "trimpod_ui.h"
#include "trimpod_autoeq.h"

extern struct eq_band_setting eq_defaults[EQ_NUM_BANDS];

#define AUTOEQ_DIR             ROCKBOX_DIR "/autoeq"
#define AUTOEQ_INDEX_PATH      ROCKBOX_DIR "/autoeq/INDEX.md"
#define AUTOEQ_CA_PATH         ROCKBOX_DIR "/autoeq/ca-bundle.crt"

#define AUTOEQ_INDEX_MAX_BYTES (4 * 1024 * 1024)
#define AUTOEQ_PEQ_MAX_BYTES   16384

struct autoeq_mirror
{
    const char *host;
    const char *results_prefix;
};

/* Three independent CDNs / ASNs so ISP blocks on Fastly (`raw.githubusercontent.com`)
 * or any single CDN never prevent downloading presets:
 *  0: GitHub REST API (`140.82.121.x`, exact endpoint NextUI & Pak Store use)
 *  1: jsDelivr multi-CDN (Cloudflare / Bunny / Fastly / GCore)
 *  2: GitHub Raw Fastly CDN (`185.199.108.x`) */
static const struct autoeq_mirror s_mirrors[] = {
    { "api.github.com",            "/repos/jaakkopasanen/AutoEq/contents/results/" },
    { "cdn.jsdelivr.net",          "/gh/jaakkopasanen/AutoEq@master/results/"      },
    { "raw.githubusercontent.com", "/jaakkopasanen/AutoEq/master/results/"         },
};
#define AUTOEQ_MIRROR_COUNT ((int)(sizeof(s_mirrors) / sizeof(s_mirrors[0])))

static int s_preferred_mirror = 0;

static int clamp_int(int v, int lo, int hi)
{
    if (v < lo) return lo;
    if (v > hi) return hi;
    return v;
}

static char ascii_lower(char c)
{
    return (c >= 'A' && c <= 'Z') ? (char)(c - 'A' + 'a') : c;
}

static bool is_alnum_ascii(char c)
{
    return (c >= '0' && c <= '9') ||
           (c >= 'a' && c <= 'z') ||
           (c >= 'A' && c <= 'Z');
}

static void make_alnum_lower(const char *src, char *dst, size_t cap)
{
    size_t w = 0;
    if (cap == 0)
        return;
    for (const char *p = src; *p && w + 1 < cap; p++)
    {
        if (is_alnum_ascii(*p))
            dst[w++] = ascii_lower(*p);
    }
    dst[w] = '\0';
}

static bool contains_ci(const char *haystack, const char *needle)
{
    size_t nlen = strlen(needle);
    if (nlen == 0)
        return true;
    for (const char *h = haystack; *h; h++)
    {
        if (strncasecmp(h, needle, nlen) == 0)
            return true;
    }
    return false;
}

static void ensure_dir_exists(const char *path)
{
    if (!dir_exists(path))
        mkdir(path);
}

/* ---- Locale-independent decimal parser for ParametricEQ.txt ------------- */

/* Parse a signed decimal number at `*p`, returning `round(val * scale)`.
 * Advances `*p` past the number; sets `*ok = true` if at least one digit read. */
static int parse_scaled_int(const char **p, int scale, bool *ok)
{
    const char *s = *p;
    while (*s == ' ' || *s == '\t')
        s++;

    int sign = 1;
    if (*s == '-')
    {
        sign = -1;
        s++;
    }
    else if (*s == '+')
    {
        s++;
    }

    long whole = 0;
    bool have_digit = false;
    while (*s >= '0' && *s <= '9')
    {
        whole = whole * 10 + (*s - '0');
        have_digit = true;
        s++;
    }

    long frac_num = 0;
    long frac_den = 1;
    if (*s == '.')
    {
        s++;
        while (*s >= '0' && *s <= '9')
        {
            have_digit = true;
            if (frac_den < 100000)
            {
                frac_num = frac_num * 10 + (*s - '0');
                frac_den *= 10;
            }
            s++;
        }
    }

    *p = s;
    *ok = have_digit;
    if (!have_digit)
        return 0;

    long scaled = whole * scale + (frac_num * scale + frac_den / 2) / frac_den;
    return (int)(sign * scaled);
}

enum peq_ftype { PEQ_NONE = 0, PEQ_LS, PEQ_PK, PEQ_HS };

struct peq_filter
{
    enum peq_ftype type;
    int cutoff;
    int q;
    int gain;
};

static int cmp_pk_abs_gain_desc(const void *a, const void *b)
{
    const struct peq_filter *fa = a, *fb = b;
    int ga = abs(fa->gain), gb = abs(fb->gain);
    if (ga != gb)
        return (gb > ga) ? 1 : -1;
    return (fa->cutoff > fb->cutoff) - (fa->cutoff < fb->cutoff);
}

static int cmp_pk_cutoff_asc(const void *a, const void *b)
{
    const struct peq_filter *fa = a, *fb = b;
    return (fa->cutoff > fb->cutoff) - (fa->cutoff < fb->cutoff);
}

bool trimpod_autoeq_parse_parametric_eq(
    const char *txt,
    int *out_precut,
    struct eq_band_setting out_bands[EQ_NUM_BANDS])
{
    if (!txt || !out_precut || !out_bands)
        return false;

    for (int i = 0; i < EQ_NUM_BANDS; i++)
    {
        out_bands[i].cutoff = eq_defaults[i].cutoff;
        out_bands[i].q      = eq_defaults[i].q;
        out_bands[i].gain   = 0;
    }
    *out_precut = 0;

    bool have_preamp = false;
    bool have_ls = false, have_hs = false;
    struct peq_filter best_ls = {0}, best_hs = {0};
    struct peq_filter pks[32];
    int npk = 0;
    int parsed_filters = 0;

    const char *cur = txt;
    while (*cur)
    {
        const char *eol = strchr(cur, '\n');
        size_t len = eol ? (size_t)(eol - cur) : strlen(cur);
        char line[256];
        if (len >= sizeof(line))
            len = sizeof(line) - 1;
        memcpy(line, cur, len);
        while (len > 0 && (line[len - 1] == '\r' || line[len - 1] == ' ' ||
                           line[len - 1] == '\t'))
            len--;
        line[len] = '\0';
        cur = eol ? eol + 1 : cur + strlen(cur);

        const char *p = line;
        while (*p == ' ' || *p == '\t')
            p++;
        if (*p == '\0' || *p == '#')
            continue;

        if (strncasecmp(p, "Preamp:", 7) == 0)
        {
            p += 7;
            bool ok = false;
            int preamp_tenths = parse_scaled_int(&p, 10, &ok);
            if (ok)
            {
                have_preamp = true;
                *out_precut = (preamp_tenths < 0)
                            ? clamp_int(-preamp_tenths, 0, 240)
                            : 0;
            }
            continue;
        }

        if (strncasecmp(p, "Filter", 6) == 0)
        {
            const char *colon = strchr(p, ':');
            if (!colon)
                continue;
            p = colon + 1;
            while (*p == ' ' || *p == '\t')
                p++;

            if (strncasecmp(p, "ON", 2) != 0 ||
                (p[2] != ' ' && p[2] != '\t'))
                continue;
            p += 2;
            while (*p == ' ' || *p == '\t')
                p++;

            char ttok[16];
            size_t ti = 0;
            while (*p && *p != ' ' && *p != '\t' && ti + 1 < sizeof(ttok))
                ttok[ti++] = *p++;
            ttok[ti] = '\0';

            enum peq_ftype ftype = PEQ_NONE;
            if (strcasecmp(ttok, "LSC") == 0 || strcasecmp(ttok, "LS") == 0 ||
                strcasecmp(ttok, "LSQ") == 0)
                ftype = PEQ_LS;
            else if (strcasecmp(ttok, "PK") == 0 || strcasecmp(ttok, "PEQ") == 0)
                ftype = PEQ_PK;
            else if (strcasecmp(ttok, "HSC") == 0 || strcasecmp(ttok, "HS") == 0 ||
                     strcasecmp(ttok, "HSQ") == 0)
                ftype = PEQ_HS;

            if (ftype == PEQ_NONE)
                continue;

            int fc = 0, gain = 0, q = 7;
            bool got_fc = false, got_gain = false;

            while (*p)
            {
                while (*p == ' ' || *p == '\t')
                    p++;
                if (!*p)
                    break;

                if (strncasecmp(p, "Fc", 2) == 0 && (p[2] == ' ' || p[2] == '\t'))
                {
                    p += 2;
                    fc = parse_scaled_int(&p, 1, &got_fc);
                }
                else if (strncasecmp(p, "Gain", 4) == 0 && (p[4] == ' ' || p[4] == '\t'))
                {
                    p += 4;
                    gain = parse_scaled_int(&p, 10, &got_gain);
                }
                else if ((p[0] == 'Q' || p[0] == 'q') && (p[1] == ' ' || p[1] == '\t'))
                {
                    p += 1;
                    bool got_q = false;
                    int qv = parse_scaled_int(&p, 10, &got_q);
                    if (got_q)
                        q = qv;
                }
                else
                {
                    while (*p && *p != ' ' && *p != '\t')
                        p++;
                }
            }

            if (!got_fc || !got_gain)
                continue;

            struct peq_filter f = {
                .type   = ftype,
                .cutoff = clamp_int(fc, EQ_CUTOFF_MIN, EQ_CUTOFF_MAX),
                .q      = clamp_int(q, EQ_Q_MIN, EQ_Q_MAX),
                .gain   = clamp_int(gain, EQ_GAIN_MIN, EQ_GAIN_MAX),
            };
            parsed_filters++;

            if (ftype == PEQ_LS)
            {
                if (!have_ls || abs(f.gain) > abs(best_ls.gain))
                {
                    best_ls = f;
                    have_ls = true;
                }
            }
            else if (ftype == PEQ_HS)
            {
                if (!have_hs || abs(f.gain) > abs(best_hs.gain))
                {
                    best_hs = f;
                    have_hs = true;
                }
            }
            else if (ftype == PEQ_PK && npk < (int)ARRAYLEN(pks))
            {
                pks[npk++] = f;
            }
        }
    }

    if (parsed_filters == 0 && !have_preamp)
        return false;

    if (have_ls)
    {
        out_bands[0].cutoff = best_ls.cutoff;
        out_bands[0].q      = best_ls.q;
        out_bands[0].gain   = best_ls.gain;
    }
    if (have_hs)
    {
        out_bands[9].cutoff = best_hs.cutoff;
        out_bands[9].q      = best_hs.q;
        out_bands[9].gain   = best_hs.gain;
    }

    if (npk > 8)
    {
        qsort(pks, (size_t)npk, sizeof(pks[0]), cmp_pk_abs_gain_desc);
        npk = 8;
    }
    if (npk > 1)
        qsort(pks, (size_t)npk, sizeof(pks[0]), cmp_pk_cutoff_asc);

    for (int i = 0; i < npk; i++)
    {
        out_bands[1 + i].cutoff = pks[i].cutoff;
        out_bands[1 + i].q      = pks[i].q;
        out_bands[1 + i].gain   = pks[i].gain;
    }
    return true;
}

static bool write_rockbox_eq_cfg(const char *path,
                                 const char *label,
                                 int precut,
                                 const struct eq_band_setting bands[EQ_NUM_BANDS])
{
    static const char *const band_names[EQ_NUM_BANDS] = {
        "eq low shelf filter",
        "eq peak filter 1",
        "eq peak filter 2",
        "eq peak filter 3",
        "eq peak filter 4",
        "eq peak filter 5",
        "eq peak filter 6",
        "eq peak filter 7",
        "eq peak filter 8",
        "eq high shelf filter",
    };

    ensure_dir_exists(EQS_DIR);
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
        return false;

    /* Keep comment lines under 120 bytes so settings_load_config's 128-byte
     * read_line buffer never splits a comment across two reads. */
    char hdr[116];
    snprintf(hdr, sizeof(hdr), "# AutoEQ: %.96s\r\n", label ? label : "");
    write(fd, hdr, strlen(hdr));

    fdprintf(fd, "eq enabled: on\r\n");
    fdprintf(fd, "eq precut: %d\r\n", precut);
    for (int i = 0; i < EQ_NUM_BANDS; i++)
    {
        fdprintf(fd, "%s: %d, %d, %d\r\n",
                 band_names[i],
                 bands[i].cutoff,
                 bands[i].q,
                 bands[i].gain);
    }
    close(fd);
    return true;
}

static void build_preset_cfg_path(const char *name, const char *source,
                                  char *out_path, size_t cap)
{
    char stem[180];
    snprintf(stem, sizeof(stem), "%s (%s)", name, source);
    for (char *p = stem; *p; p++)
    {
        if (*p == '/' || *p == '\\' || *p == ':' || *p == '*' ||
            *p == '?' || *p == '"'  || *p == '<' || *p == '>' || *p == '|')
            *p = '_';
    }
    snprintf(out_path, cap, "%s/%s.cfg", EQS_DIR, stem);
}

/* ---- AutoEQ index loading & ranked search ------------------------------- */

struct autoeq_entry
{
    const char *name;
    const char *source;
    const char *rel_url;
};

struct autoeq_match
{
    int idx;
    int rank;
};

static char                *s_index_buf   = NULL;
static struct autoeq_entry *s_entries     = NULL;
static int                  s_entry_count = 0;

static struct autoeq_match *s_matches     = NULL;
static int                  s_match_count = 0;
static char                 s_query[128]  = "";

static void autoeq_free_index(void)
{
    free(s_matches);
    s_matches = NULL;
    s_match_count = 0;

    free(s_entries);
    s_entries = NULL;
    s_entry_count = 0;

    free(s_index_buf);
    s_index_buf = NULL;
}

static char *rfind_substr(char *start, size_t len, const char *pat)
{
    size_t plen = strlen(pat);
    if (len < plen)
        return NULL;
    for (size_t i = len - plen + 1; i-- > 0; )
    {
        if (memcmp(start + i, pat, plen) == 0)
            return start + i;
    }
    return NULL;
}

static bool autoeq_load_index(void)
{
    if (s_entries && s_entry_count > 0)
        return true;

    autoeq_free_index();

    int fd = open(AUTOEQ_INDEX_PATH, O_RDONLY);
    if (fd < 0)
        return false;

    off_t sz = filesize(fd);
    if (sz <= 0 || sz > AUTOEQ_INDEX_MAX_BYTES)
    {
        close(fd);
        return false;
    }

    s_index_buf = malloc((size_t)sz + 1);
    if (!s_index_buf)
    {
        close(fd);
        return false;
    }

    ssize_t nread = read(fd, s_index_buf, (size_t)sz);
    close(fd);
    if (nread <= 0)
    {
        autoeq_free_index();
        return false;
    }
    s_index_buf[nread] = '\0';

    /* Count "- [" entries first for a single exact allocation. */
    int cap = 0;
    for (char *p = s_index_buf; *p; )
    {
        char *eol = strchr(p, '\n');
        if (strncmp(p, "- [", 3) == 0)
            cap++;
        if (!eol)
            break;
        p = eol + 1;
    }
    if (cap == 0)
    {
        autoeq_free_index();
        return false;
    }

    s_entries = malloc((size_t)cap * sizeof(*s_entries));
    s_matches = malloc((size_t)cap * sizeof(*s_matches));
    if (!s_entries || !s_matches)
    {
        autoeq_free_index();
        return false;
    }

    char *cur = s_index_buf;
    while (*cur && s_entry_count < cap)
    {
        char *eol = strchr(cur, '\n');
        size_t len = eol ? (size_t)(eol - cur) : strlen(cur);
        if (len > 0 && cur[len - 1] == '\r')
            cur[--len] = '\0';
        else if (eol)
            *eol = '\0';

        if (strncmp(cur, "- [", 3) == 0)
        {
            char *mid = strstr(cur + 3, "](./");
            char *by  = mid ? rfind_substr(mid + 4,
                                           len - (size_t)((mid + 4) - cur),
                                           ") by ")
                            : NULL;
            if (mid && by && by > mid + 4)
            {
                *mid = '\0';
                *by  = '\0';
                s_entries[s_entry_count].name    = cur + 3;
                s_entries[s_entry_count].rel_url = mid + 4;
                s_entries[s_entry_count].source  = by + 5;
                s_entry_count++;
            }
        }
        if (!eol)
            break;
        cur = eol + 1;
    }

    if (s_entry_count == 0)
    {
        autoeq_free_index();
        return false;
    }
    return true;
}

static int source_priority(const char *src)
{
    if (strncasecmp(src, "oratory1990", 11) == 0)   return 0;
    if (strncasecmp(src, "crinacle", 8) == 0)       return 1;
    if (strncasecmp(src, "Super Review", 12) == 0)  return 2;
    if (strncasecmp(src, "Resolve", 7) == 0)        return 3;
    if (strncasecmp(src, "Rtings", 6) == 0)         return 4;
    if (strncasecmp(src, "Innerfidelity", 13) == 0) return 5;
    return 6;
}

static int cmp_autoeq_matches(const void *a, const void *b)
{
    const struct autoeq_match *ma = a, *mb = b;
    if (ma->rank != mb->rank)
        return (ma->rank > mb->rank) - (ma->rank < mb->rank);

    const struct autoeq_entry *ea = &s_entries[ma->idx];
    const struct autoeq_entry *eb = &s_entries[mb->idx];

    if (s_query[0] != '\0')
    {
        size_t la = strlen(ea->name), lb = strlen(eb->name);
        int name_cmp = strnatcasecmp(ea->name, eb->name);
        if (name_cmp == 0)
        {
            int pa = source_priority(ea->source);
            int pb = source_priority(eb->source);
            if (pa != pb)
                return pa - pb;
            return strnatcasecmp(ea->source, eb->source);
        }
        if (la != lb && ma->rank <= 2)
            return (la > lb) ? 1 : -1;
        return name_cmp;
    }
    int nc = strnatcasecmp(ea->name, eb->name);
    if (nc != 0)
        return nc;
    return source_priority(ea->source) - source_priority(eb->source);
}

#define MAX_QUERY_TOKENS 8

static void autoeq_filter(void)
{
    s_match_count = 0;
    if (!s_entries || !s_matches || s_entry_count <= 0)
        return;

    char qcopy[128];
    strlcpy(qcopy, s_query, sizeof(qcopy));

    char *toks[MAX_QUERY_TOKENS];
    char tok_alnum[MAX_QUERY_TOKENS][64];
    int ntok = 0;

    char *p = qcopy;
    while (*p && ntok < MAX_QUERY_TOKENS)
    {
        while (*p == ' ' || *p == '\t')
            p++;
        if (!*p)
            break;
        toks[ntok] = p;
        while (*p && *p != ' ' && *p != '\t')
            p++;
        if (*p)
            *p++ = '\0';
        make_alnum_lower(toks[ntok], tok_alnum[ntok], sizeof(tok_alnum[ntok]));
        ntok++;
    }

    if (ntok == 0)
    {
        for (int i = 0; i < s_entry_count; i++)
        {
            s_matches[i].idx  = i;
            s_matches[i].rank = 0;
        }
        s_match_count = s_entry_count;
        return;
    }

    char q_trimmed[128];
    const char *qs = s_query;
    while (*qs == ' ')
        qs++;
    strlcpy(q_trimmed, qs, sizeof(q_trimmed));
    size_t qlen = strlen(q_trimmed);
    while (qlen > 0 && q_trimmed[qlen - 1] == ' ')
        q_trimmed[--qlen] = '\0';

    char q_alnum[128];
    make_alnum_lower(q_trimmed, q_alnum, sizeof(q_alnum));
    size_t qa_len = strlen(q_alnum);

    for (int i = 0; i < s_entry_count; i++)
    {
        const struct autoeq_entry *e = &s_entries[i];
        char name_alnum[160];
        char src_alnum[96];
        make_alnum_lower(e->name, name_alnum, sizeof(name_alnum));
        make_alnum_lower(e->source, src_alnum, sizeof(src_alnum));

        bool all_ok = true;
        bool all_in_name = true;

        for (int k = 0; k < ntok; k++)
        {
            bool in_name = contains_ci(e->name, toks[k]) ||
                           (tok_alnum[k][0] && strstr(name_alnum, tok_alnum[k]) != NULL);
            if (in_name)
                continue;
            all_in_name = false;
            bool in_src = contains_ci(e->source, toks[k]) ||
                          (tok_alnum[k][0] && strstr(src_alnum, tok_alnum[k]) != NULL);
            if (!in_src)
            {
                all_ok = false;
                break;
            }
        }
        if (!all_ok)
            continue;

        int rank = 4;
        if (strcasecmp(e->name, q_trimmed) == 0 ||
            (qa_len > 0 && strcmp(name_alnum, q_alnum) == 0))
            rank = 0;
        else if (strncasecmp(e->name, q_trimmed, qlen) == 0 ||
                 (qa_len > 0 && strncmp(name_alnum, q_alnum, qa_len) == 0))
            rank = 1;
        else if (qa_len > 0 && strstr(name_alnum, q_alnum) != NULL)
            rank = 2;
        else if (all_in_name)
            rank = 3;

        s_matches[s_match_count].idx  = i;
        s_matches[s_match_count].rank = rank;
        s_match_count++;
    }

    if (s_match_count > 1)
        qsort(s_matches, (size_t)s_match_count, sizeof(s_matches[0]),
              cmp_autoeq_matches);
}

/* ---- Downloading a profile or refreshing INDEX.md ----------------------- */

/* Copy `src` into `dst`, encoding literal '+' as "%2B" so all CDNs/APIs treat
 * '+' as part of the filename (e.g. "4+2") rather than a space. */
static bool encode_plus_segment(const char *src, char *dst, size_t cap)
{
    size_t w = 0;
    for (const char *p = src; *p; p++)
    {
        if (*p == '+')
        {
            if (w + 3 >= cap)
                return false;
            dst[w++] = '%';
            dst[w++] = '2';
            dst[w++] = 'B';
        }
        else
        {
            if (w + 1 >= cap)
                return false;
            dst[w++] = *p;
        }
    }
    dst[w] = '\0';
    return true;
}

static bool autoeq_refresh_database(void)
{
    ensure_dir_exists(AUTOEQ_DIR);
    splash(0, ID2P(LANG_TRIMPOD_AUTOEQ_DOWNLOADING));

    bool ok = false;
    for (int step = 0; step < AUTOEQ_MIRROR_COUNT; step++)
    {
        int m = (s_preferred_mirror + step) % AUTOEQ_MIRROR_COUNT;
        char uri[256];
        snprintf(uri, sizeof(uri), "%sINDEX.md", s_mirrors[m].results_prefix);

        bool allow_curl = (step == AUTOEQ_MIRROR_COUNT - 1);
        if (retrohh_https_download_file_ex(s_mirrors[m].host, uri,
                                           AUTOEQ_CA_PATH, allow_curl,
                                           AUTOEQ_INDEX_PATH,
                                           AUTOEQ_INDEX_MAX_BYTES))
        {
            s_preferred_mirror = m;
            ok = true;
            break;
        }
        if (retrohh_https_network_down())
            break;
    }

    if (!ok)
    {
        splashf(HZ * 5 / 2, "Download failed (%s)", retrohh_https_last_error());
        return false;
    }

    autoeq_free_index();
    if (!autoeq_load_index())
    {
        splash(HZ * 2, ID2P(LANG_FAILED));
        return false;
    }
    autoeq_filter();
    splash(HZ, ID2P(LANG_TRIMPOD_AUTOEQ_UPDATED));
    return true;
}

static bool autoeq_download_and_save(const struct autoeq_entry *e, bool apply_now)
{
    const char *slash = strrchr(e->rel_url, '/');
    const char *last_seg = slash ? slash + 1 : e->rel_url;

    char rel_enc[512];
    char seg_enc[256];
    if (!encode_plus_segment(e->rel_url, rel_enc, sizeof(rel_enc)) ||
        !encode_plus_segment(last_seg, seg_enc, sizeof(seg_enc)))
    {
        splash(HZ, ID2P(LANG_FAILED));
        return false;
    }

    splash(0, ID2P(LANG_TRIMPOD_AUTOEQ_DOWNLOADING));

    char *peq_txt = malloc(AUTOEQ_PEQ_MAX_BYTES);
    if (!peq_txt)
    {
        splash(HZ, ID2P(LANG_FAILED));
        return false;
    }

    size_t peq_len = 0;
    bool fetched = false;

    for (int step = 0; step < AUTOEQ_MIRROR_COUNT; step++)
    {
        int m = (s_preferred_mirror + step) % AUTOEQ_MIRROR_COUNT;
        char uri[1024];
        int n = snprintf(uri, sizeof(uri), "%s%s/%s%%20ParametricEQ.txt",
                         s_mirrors[m].results_prefix, rel_enc, seg_enc);
        if (n <= 0 || (size_t)n >= sizeof(uri))
            continue;

        bool allow_curl = (step == AUTOEQ_MIRROR_COUNT - 1);
        peq_len = 0;
        if (retrohh_https_get_ex(s_mirrors[m].host, uri, AUTOEQ_CA_PATH,
                                 allow_curl, peq_txt, AUTOEQ_PEQ_MAX_BYTES,
                                 &peq_len) && peq_len > 0)
        {
            s_preferred_mirror = m;
            fetched = true;
            break;
        }
        if (retrohh_https_network_down())
            break;
    }

    if (!fetched || peq_len == 0)
    {
        free(peq_txt);
        splashf(HZ * 5 / 2, "Download failed (%s)", retrohh_https_last_error());
        return false;
    }

    int precut = 0;
    struct eq_band_setting bands[EQ_NUM_BANDS];
    bool parsed = trimpod_autoeq_parse_parametric_eq(peq_txt, &precut, bands);
    free(peq_txt);

    if (!parsed)
    {
        splash(HZ * 2, ID2P(LANG_FAILED));
        return false;
    }

    char cfg_path[MAX_PATH];
    build_preset_cfg_path(e->name, e->source, cfg_path, sizeof(cfg_path));

    char label[160];
    snprintf(label, sizeof(label), "%s (%s)", e->name, e->source);

    if (!write_rockbox_eq_cfg(cfg_path, label, precut, bands))
    {
        splash(HZ * 2, ID2P(LANG_FAILED));
        return false;
    }

    if (apply_now)
    {
        if (settings_load_config(cfg_path, true))
            splash(HZ * 3 / 2, ID2P(LANG_TRIMPOD_AUTOEQ_SAVED_APPLIED));
        else
            splash(HZ, ID2P(LANG_FAILED));
    }
    else
    {
        splash(HZ, ID2P(LANG_SETTINGS_SAVED));
    }
    return true;
}

/* ---- AutoEQ page (gui_synclist + trimpod_page) -------------------------- */

struct autoeq_page
{
    struct trimpod_page base;
    struct gui_synclist lists;
};

static bool autoeq_showing_empty_placeholder(void)
{
    return (s_query[0] != '\0' && s_match_count == 0);
}

static int autoeq_row_count(void)
{
    /* Row 0: Search... / Search: <query>
     * Rows 1..M: matches (or 1 "(No matches)" row when filtered to 0)
     * Last row: Update AutoEQ Database */
    int mid = autoeq_showing_empty_placeholder() ? 1 : s_match_count;
    return 1 + mid + 1;
}

static int autoeq_update_row_index(void)
{
    return autoeq_row_count() - 1;
}

static const char *autoeq_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)data;
    if (sel == 0)
    {
        if (s_query[0] == '\0')
            return str(LANG_TRIMPOD_AUTOEQ_SEARCH);
        snprintf(buf, buf_len, "Search: %s", s_query);
        return buf;
    }
    if (sel == autoeq_update_row_index())
        return str(LANG_TRIMPOD_AUTOEQ_UPDATE);
    if (autoeq_showing_empty_placeholder() && sel == 1)
        return str(LANG_TRIMPOD_AUTOEQ_NO_MATCHES);

    int m = sel - 1;
    if (m < 0 || m >= s_match_count)
        return "";
    const struct autoeq_entry *e = &s_entries[s_matches[m].idx];
    snprintf(buf, buf_len, "%s (%s)", e->name, e->source);
    return buf;
}

static void autoeq_draw(struct trimpod_page *self)
{
    gui_synclist_draw(&((struct autoeq_page *)self)->lists);
}

static int autoeq_poll(struct trimpod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct autoeq_page *)self)->lists, &action);
    return action;
}

static void autoeq_open_search(struct autoeq_page *p)
{
    char buf[sizeof(s_query)];
    strlcpy(buf, s_query, sizeof(buf));
    if (trimpod_kbd_input(buf, sizeof(buf)) != 0)
        return;

    strlcpy(s_query, buf, sizeof(s_query));
    autoeq_filter();
    gui_synclist_set_nb_items(&p->lists, autoeq_row_count());
    if (s_query[0] != '\0' && s_match_count > 0)
        gui_synclist_select_item(&p->lists, 1);
    else
        gui_synclist_select_item(&p->lists, 0);
}

static enum trimpod_page_result autoeq_on_action(struct trimpod_page *self,
                                                 int action)
{
    struct autoeq_page *p = (struct autoeq_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL)
    {
        trimpod_transition_arm_back();
        return TRIMPOD_PAGE_DONE;
    }

    if (action == ACTION_STD_OK)
    {
        if (sel == 0)
        {
            autoeq_open_search(p);
        }
        else if (sel == autoeq_update_row_index())
        {
            if (autoeq_refresh_database())
            {
                gui_synclist_set_nb_items(&p->lists, autoeq_row_count());
                gui_synclist_select_item(&p->lists, 0);
            }
        }
        else if (!autoeq_showing_empty_placeholder())
        {
            int m = sel - 1;
            if (m >= 0 && m < s_match_count)
                autoeq_download_and_save(&s_entries[s_matches[m].idx], true);
        }
        return TRIMPOD_PAGE_STAY;
    }

    if (action == ACTION_STD_CONTEXT)
    {
        if (sel == 0 && s_query[0] != '\0')
        {
            s_query[0] = '\0';
            autoeq_filter();
            gui_synclist_set_nb_items(&p->lists, autoeq_row_count());
            gui_synclist_select_item(&p->lists, 0);
        }
        else if (!autoeq_showing_empty_placeholder() &&
                 sel >= 1 && sel <= s_match_count)
        {
            const struct autoeq_entry *e = &s_entries[s_matches[sel - 1].idx];
            static const char *const opts[] = {
                ID2P(LANG_TRIMPOD_AUTOEQ_APPLY),
                ID2P(LANG_TRIMPOD_AUTOEQ_SAVE_ONLY),
            };
            int choice = trimpod_context_menu(e->name, opts, ARRAYLEN(opts));
            if (choice == 0)
                autoeq_download_and_save(e, true);
            else if (choice == 1)
                autoeq_download_and_save(e, false);
        }
        return TRIMPOD_PAGE_STAY;
    }

    return TRIMPOD_PAGE_STAY;
}

static const struct trimpod_page_vtable autoeq_vtable =
{
    .legend    = NULL,
    .draw      = autoeq_draw,
    .poll      = autoeq_poll,
    .on_action = autoeq_on_action,
};

static const int autoeq_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CONTEXT, ACTION_STD_CANCEL, -1
};

int trimpod_autoeq_page(void)
{
    ensure_dir_exists(EQS_DIR);
    if (!autoeq_load_index())
        autoeq_refresh_database();

    autoeq_filter();

    struct autoeq_page p =
    {
        .base = {
            .vt      = &autoeq_vtable,
            .context = CONTEXT_LIST,
            .allowed = autoeq_allowed,
            .title   = str(LANG_TRIMPOD_AUTOEQ),
        },
    };
    gui_synclist_init(&p.lists, autoeq_get_name, NULL, false, 1, NULL);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_TRIMPOD_AUTOEQ), Icon_EQ);
    gui_synclist_set_nb_items(&p.lists, autoeq_row_count());
    gui_synclist_select_item(&p.lists,
                             (s_query[0] != '\0' && s_match_count > 0) ? 1 : 0);

    trimpod_page_run(&p.base);
    return p.base.home ? GO_TO_ROOT : 0;
}

/* ---- Browse EQ Presets page (local EQS_DIR .cfg files) ------------------ */

struct eq_presets_page
{
    struct trimpod_page  base;
    struct gui_synclist  lists;
    char               **files;
    int                  count;
    int                  cap;
};

static void presets_free(struct eq_presets_page *p)
{
    for (int i = 0; i < p->count; i++)
        free(p->files[i]);
    free(p->files);
    p->files = NULL;
    p->count = 0;
    p->cap   = 0;
}

static int cmp_preset_names(const void *a, const void *b)
{
    const char * const *sa = a;
    const char * const *sb = b;
    return strnatcasecmp(*sa, *sb);
}

static void presets_scan(struct eq_presets_page *p)
{
    presets_free(p);
    ensure_dir_exists(EQS_DIR);

    DIR *d = opendir(EQS_DIR);
    if (!d)
        return;

    struct dirent *e;
    while ((e = readdir(d)) != NULL)
    {
        if (e->d_name[0] == '.')
            continue;
        size_t len = strlen(e->d_name);
        if (len <= 4 || strcasecmp(e->d_name + len - 4, ".cfg") != 0)
            continue;

        if (p->count == p->cap)
        {
            int ncap = p->cap ? p->cap * 2 : 32;
            char **nf = realloc(p->files, (size_t)ncap * sizeof(*nf));
            if (!nf)
                break;
            p->files = nf;
            p->cap   = ncap;
        }
        size_t slen = len + 1;
        char *copy = malloc(slen);
        if (!copy)
            break;
        memcpy(copy, e->d_name, slen);
        p->files[p->count++] = copy;
    }
    closedir(d);

    if (p->count > 1)
        qsort(p->files, (size_t)p->count, sizeof(p->files[0]), cmp_preset_names);
}

static void preset_display_name(const char *filename, char *out, size_t cap)
{
    strlcpy(out, filename, cap);
    size_t len = strlen(out);
    if (len > 4 && strcasecmp(out + len - 4, ".cfg") == 0)
        out[len - 4] = '\0';
}

static const char *presets_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    struct eq_presets_page *p = data;
    if (!p || sel < 0 || sel >= p->count)
        return "";
    preset_display_name(p->files[sel], buf, buf_len);
    return buf;
}

static void presets_draw(struct trimpod_page *self)
{
    gui_synclist_draw(&((struct eq_presets_page *)self)->lists);
}

static int presets_poll(struct trimpod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct eq_presets_page *)self)->lists, &action);
    return action;
}

static void preset_load_index(struct eq_presets_page *p, int sel)
{
    if (sel < 0 || sel >= p->count)
        return;
    char full[MAX_PATH];
    path_append(full, EQS_DIR, p->files[sel], sizeof(full));
    if (settings_load_config(full, true))
        splash(HZ, ID2P(LANG_SETTINGS_LOADED));
    else
        splash(HZ, ID2P(LANG_FAILED));
}

static enum trimpod_page_result presets_on_action(struct trimpod_page *self,
                                                  int action)
{
    struct eq_presets_page *p = (struct eq_presets_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL)
    {
        trimpod_transition_arm_back();
        return TRIMPOD_PAGE_DONE;
    }

    if (action == ACTION_STD_OK)
    {
        preset_load_index(p, sel);
        return TRIMPOD_PAGE_STAY;
    }

    if (action == ACTION_STD_CONTEXT && sel >= 0 && sel < p->count)
    {
        char disp[MAX_PATH];
        preset_display_name(p->files[sel], disp, sizeof(disp));

        static const char *const opts[] = {
            ID2P(LANG_TRIMPOD_APPLY_PRESET),
            ID2P(LANG_DELETE),
        };
        int choice = trimpod_context_menu(disp, opts, ARRAYLEN(opts));
        if (choice == 0)
        {
            preset_load_index(p, sel);
        }
        else if (choice == 1)
        {
            if (trimpod_confirm(str(LANG_TRIMPOD_DELETE_PRESET_Q), disp))
            {
                char full[MAX_PATH];
                path_append(full, EQS_DIR, p->files[sel], sizeof(full));
                remove(full);
                presets_scan(p);
                if (p->count == 0)
                {
                    trimpod_transition_arm_back();
                    return TRIMPOD_PAGE_DONE;
                }
                gui_synclist_set_nb_items(&p->lists, p->count);
                if (sel >= p->count)
                    gui_synclist_select_item(&p->lists, p->count - 1);
            }
        }
        return TRIMPOD_PAGE_STAY;
    }

    return TRIMPOD_PAGE_STAY;
}

static const struct trimpod_page_vtable presets_vtable =
{
    .legend    = NULL,
    .draw      = presets_draw,
    .poll      = presets_poll,
    .on_action = presets_on_action,
};

int trimpod_eq_presets_page(void)
{
    struct eq_presets_page p =
    {
        .base = {
            .vt      = &presets_vtable,
            .context = CONTEXT_LIST,
            .allowed = autoeq_allowed,
            .title   = str(LANG_EQUALIZER_BROWSE),
        },
    };

    presets_scan(&p);
    if (p.count == 0)
    {
        presets_free(&p);
        splash(HZ * 3 / 2, ID2P(LANG_NO_FILES));
        trimpod_transition_suppress_next();
        return 0;
    }

    gui_synclist_init(&p.lists, presets_get_name, &p, false, 1, NULL);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_EQUALIZER_BROWSE), Icon_EQ);
    gui_synclist_set_nb_items(&p.lists, p.count);
    gui_synclist_select_item(&p.lists, 0);

    trimpod_page_run(&p.base);
    presets_free(&p);
    return p.base.home ? GO_TO_ROOT : 0;
}
