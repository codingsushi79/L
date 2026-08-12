/* The L runtime.
 *
 * This file is embedded in the compiler and prepended to every generated C
 * translation unit, so a compiled program is one self-contained file and needs
 * no separate runtime library to be installed alongside it.
 *
 * Memory: values are allocated and never released. SPEC §70 states that the
 * ownership and memory model must be settled before 1.0 is final, so this
 * compiler deliberately does not commit to one; it leaks. That is sound but
 * unbounded, and is the single largest gap between this implementation and the
 * specification.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <math.h>
#include <setjmp.h>
#include <time.h>

/* ---------------------------------------------------------------- memory */

static void *l_alloc(size_t n) {
    void *p = calloc(1, n ? n : 1);
    if (!p) {
        fputs("l: out of memory\n", stderr);
        exit(70);
    }
    return p;
}

static void *l_realloc(void *p, size_t n) {
    void *q = realloc(p, n ? n : 1);
    if (!q) {
        fputs("l: out of memory\n", stderr);
        exit(70);
    }
    return q;
}

/* --------------------------------------------------------------- strings */

/* SPEC §11: strings are UTF-8. They are kept NUL-terminated as well as
 * length-prefixed so that they can be handed to C library calls directly. */
typedef struct {
    char *data;
    int64_t len;
} l_str;

static l_str l_str_new(const char *src, int64_t len) {
    l_str s;
    s.data = (char *)l_alloc((size_t)len + 1);
    if (len > 0) memcpy(s.data, src, (size_t)len);
    s.data[len] = 0;
    s.len = len;
    return s;
}

static l_str l_str_lit(const char *src) {
    return l_str_new(src, (int64_t)strlen(src));
}

static const l_str L_EMPTY_STR = {(char *)"", 0};

static l_str l_str_concat2(l_str a, l_str b) {
    l_str s;
    s.len = a.len + b.len;
    s.data = (char *)l_alloc((size_t)s.len + 1);
    memcpy(s.data, a.data, (size_t)a.len);
    memcpy(s.data + a.len, b.data, (size_t)b.len);
    s.data[s.len] = 0;
    return s;
}

static l_str l_str_concat(int n, const l_str *parts) {
    int64_t total = 0;
    for (int i = 0; i < n; i++) total += parts[i].len;
    l_str s;
    s.len = total;
    s.data = (char *)l_alloc((size_t)total + 1);
    int64_t at = 0;
    for (int i = 0; i < n; i++) {
        memcpy(s.data + at, parts[i].data, (size_t)parts[i].len);
        at += parts[i].len;
    }
    s.data[total] = 0;
    return s;
}

static int8_t l_str_eq(l_str a, l_str b) {
    return a.len == b.len && memcmp(a.data, b.data, (size_t)a.len) == 0;
}

static int l_str_cmp(l_str a, l_str b) {
    int64_t n = a.len < b.len ? a.len : b.len;
    int c = n ? memcmp(a.data, b.data, (size_t)n) : 0;
    if (c) return c;
    return a.len == b.len ? 0 : (a.len < b.len ? -1 : 1);
}

/* -------------------------------------------------------------- failures */

/* SPEC §31: a failure unwinds to the nearest `try`. With no handler installed
 * the program reports the failure and stops. */
typedef struct l_handler {
    jmp_buf buf;
    struct l_handler *prev;
} l_handler;

static l_handler *l_handlers = NULL;
static l_str l_error_message = {(char *)"", 0};

static jmp_buf *l_try_push(void) {
    l_handler *h = (l_handler *)l_alloc(sizeof(l_handler));
    h->prev = l_handlers;
    l_handlers = h;
    return &h->buf;
}

static void l_try_pop(void) {
    if (l_handlers) l_handlers = l_handlers->prev;
}

static l_str l_caught(void) { return l_error_message; }

static void l_fail(l_str message) {
    l_error_message = message;
    if (l_handlers) {
        l_handler *h = l_handlers;
        l_handlers = h->prev;
        longjmp(h->buf, 1);
    }
    fputs("error: ", stderr);
    fwrite(message.data, 1, (size_t)message.len, stderr);
    fputc('\n', stderr);
    exit(1);
}

static void l_fail_cstr(const char *m) { l_fail(l_str_lit(m)); }

/* ---------------------------------------------------------------- arrays */

/* SPEC §12. Arrays are handles, so passing one to a function and appending to
 * it there is visible to the caller. */
typedef struct {
    void *data;
    int64_t len;
    int64_t cap;
    int64_t esz;
} l_array_s;

typedef l_array_s *l_array;

static l_array l_array_new(int64_t esz, int64_t cap) {
    l_array a = (l_array)l_alloc(sizeof(l_array_s));
    a->esz = esz;
    a->cap = cap > 0 ? cap : 4;
    a->len = 0;
    a->data = l_alloc((size_t)(a->cap * esz));
    return a;
}

static void l_array_reserve(l_array a, int64_t n) {
    if (n <= a->cap) return;
    int64_t cap = a->cap * 2;
    if (cap < n) cap = n;
    a->data = l_realloc(a->data, (size_t)(cap * a->esz));
    memset((char *)a->data + a->cap * a->esz, 0, (size_t)((cap - a->cap) * a->esz));
    a->cap = cap;
}

static void *l_array_at(l_array a, int64_t i) {
    if (i < 0 || i >= a->len) {
        char buf[128];
        snprintf(buf, sizeof buf,
                 "array index %lld is out of bounds for length %lld",
                 (long long)i, (long long)a->len);
        l_fail_cstr(buf);
    }
    return (char *)a->data + i * a->esz;
}

static void l_array_push(l_array a, const void *v) {
    l_array_reserve(a, a->len + 1);
    memcpy((char *)a->data + a->len * a->esz, v, (size_t)a->esz);
    a->len++;
}

static void l_array_pop(l_array a, void *out) {
    if (a->len == 0) l_fail_cstr("pop from an empty array");
    a->len--;
    memcpy(out, (char *)a->data + a->len * a->esz, (size_t)a->esz);
}

static int64_t l_array_len(l_array a) { return a ? a->len : 0; }

/* ------------------------------------------------------------------ keys */

/* Map keys and set elements are primitives (SPEC §13, §14), so one tagged
 * union covers every key type. */
enum { L_KEY_INT, L_KEY_FLOAT, L_KEY_STR, L_KEY_BOOL, L_KEY_CHAR };

typedef struct {
    int8_t kind;
    int64_t i;
    double f;
    l_str s;
} l_key;

static l_key l_key_int(int64_t v) { l_key k = {L_KEY_INT, v, 0, {(char *)"", 0}}; return k; }
static l_key l_key_float(double v) { l_key k = {L_KEY_FLOAT, 0, v, {(char *)"", 0}}; return k; }
static l_key l_key_bool(int8_t v) { l_key k = {L_KEY_BOOL, v, 0, {(char *)"", 0}}; return k; }
static l_key l_key_char(int32_t v) { l_key k = {L_KEY_CHAR, v, 0, {(char *)"", 0}}; return k; }
static l_key l_key_str(l_str v) { l_key k = {L_KEY_STR, 0, 0, {(char *)"", 0}}; k.s = v; return k; }

static int8_t l_key_eq(l_key a, l_key b) {
    if (a.kind != b.kind) return 0;
    switch (a.kind) {
        case L_KEY_STR: return l_str_eq(a.s, b.s);
        case L_KEY_FLOAT: return a.f == b.f;
        default: return a.i == b.i;
    }
}

/* ------------------------------------------------------------------ maps */

/* SPEC §13. Entries keep insertion order, which is what makes iterating a map
 * deterministic. Lookup is linear: correctness first, and a hash index can be
 * added without changing the interface. */
typedef struct {
    l_key *keys;
    char *vals;
    int64_t len;
    int64_t cap;
    int64_t vsz;
} l_map_s;

typedef l_map_s *l_map;

static l_map l_map_new(int64_t vsz) {
    l_map m = (l_map)l_alloc(sizeof(l_map_s));
    m->vsz = vsz;
    m->cap = 4;
    m->len = 0;
    m->keys = (l_key *)l_alloc(sizeof(l_key) * (size_t)m->cap);
    m->vals = (char *)l_alloc((size_t)(m->cap * vsz));
    return m;
}

static int64_t l_map_find(l_map m, l_key k) {
    for (int64_t i = 0; i < m->len; i++)
        if (l_key_eq(m->keys[i], k)) return i;
    return -1;
}

static void l_map_grow(l_map m) {
    if (m->len < m->cap) return;
    int64_t cap = m->cap * 2;
    m->keys = (l_key *)l_realloc(m->keys, sizeof(l_key) * (size_t)cap);
    m->vals = (char *)l_realloc(m->vals, (size_t)(cap * m->vsz));
    m->cap = cap;
}

static void l_map_set(l_map m, l_key k, const void *v) {
    int64_t i = l_map_find(m, k);
    if (i < 0) {
        l_map_grow(m);
        i = m->len++;
        m->keys[i] = k;
    }
    memcpy(m->vals + i * m->vsz, v, (size_t)m->vsz);
}

static void *l_map_get(l_map m, l_key k) {
    int64_t i = l_map_find(m, k);
    if (i < 0) {
        if (k.kind == L_KEY_STR) {
            l_str parts[3];
            parts[0] = l_str_lit("no entry for key \"");
            parts[1] = k.s;
            parts[2] = l_str_lit("\"");
            l_fail(l_str_concat(3, parts));
        }
        char buf[96];
        snprintf(buf, sizeof buf, "no entry for key %lld", (long long)k.i);
        l_fail_cstr(buf);
    }
    return m->vals + i * m->vsz;
}

static int8_t l_map_has(l_map m, l_key k) { return l_map_find(m, k) >= 0; }

static void l_map_remove(l_map m, l_key k) {
    int64_t i = l_map_find(m, k);
    if (i < 0) return;
    for (int64_t j = i; j + 1 < m->len; j++) {
        m->keys[j] = m->keys[j + 1];
        memcpy(m->vals + j * m->vsz, m->vals + (j + 1) * m->vsz, (size_t)m->vsz);
    }
    m->len--;
}

static int64_t l_map_len(l_map m) { return m ? m->len : 0; }

static l_key l_map_key_at(l_map m, int64_t i) {
    if (i < 0 || i >= m->len) l_fail_cstr("map iteration out of range");
    return m->keys[i];
}

static void *l_map_val_at(l_map m, int64_t i) {
    if (i < 0 || i >= m->len) l_fail_cstr("map iteration out of range");
    return m->vals + i * m->vsz;
}

/* ------------------------------------------------------------------ sets */

/* SPEC §14, with the same ordering guarantee as maps. */
typedef struct {
    l_key *keys;
    int64_t len;
    int64_t cap;
} l_set_s;

typedef l_set_s *l_set;

static l_set l_set_new(void) {
    l_set s = (l_set)l_alloc(sizeof(l_set_s));
    s->cap = 4;
    s->len = 0;
    s->keys = (l_key *)l_alloc(sizeof(l_key) * (size_t)s->cap);
    return s;
}

static int8_t l_set_has(l_set s, l_key k) {
    for (int64_t i = 0; i < s->len; i++)
        if (l_key_eq(s->keys[i], k)) return 1;
    return 0;
}

static void l_set_add(l_set s, l_key k) {
    if (l_set_has(s, k)) return;
    if (s->len >= s->cap) {
        s->cap *= 2;
        s->keys = (l_key *)l_realloc(s->keys, sizeof(l_key) * (size_t)s->cap);
    }
    s->keys[s->len++] = k;
}

static void l_set_remove(l_set s, l_key k) {
    for (int64_t i = 0; i < s->len; i++) {
        if (l_key_eq(s->keys[i], k)) {
            for (int64_t j = i; j + 1 < s->len; j++) s->keys[j] = s->keys[j + 1];
            s->len--;
            return;
        }
    }
}

static int64_t l_set_len(l_set s) { return s ? s->len : 0; }

static l_key l_set_at(l_set s, int64_t i) {
    if (i < 0 || i >= s->len) l_fail_cstr("set iteration out of range");
    return s->keys[i];
}

/* ---------------------------------------------------------------- ranges */

/* SPEC §19. Always half-open: `a..=b` was normalised to `a..(b + 1)`. */
typedef struct {
    int64_t start;
    int64_t end;
} l_range;

static l_range l_range_new(int64_t a, int64_t b) {
    l_range r = {a, b};
    return r;
}

/* --------------------------------------------------- text from any value */

static l_str l_str_from_int(int64_t v) {
    char buf[32];
    int n = snprintf(buf, sizeof buf, "%lld", (long long)v);
    return l_str_new(buf, n);
}

static l_str l_str_from_uint(uint64_t v) {
    char buf[32];
    int n = snprintf(buf, sizeof buf, "%llu", (unsigned long long)v);
    return l_str_new(buf, n);
}

static l_str l_str_from_float(double v) {
    char buf[40];
    int n;
    if (v == (int64_t)v && fabs(v) < 1e15) {
        n = snprintf(buf, sizeof buf, "%.1f", v);
    } else {
        n = snprintf(buf, sizeof buf, "%g", v);
    }
    return l_str_new(buf, n);
}

static l_str l_str_from_bool(int8_t v) {
    return l_str_lit(v ? "true" : "false");
}

/* Encodes one code point as UTF-8 (SPEC §11). */
static l_str l_str_from_char(int32_t c) {
    char buf[5];
    int n = 0;
    uint32_t u = (uint32_t)c;
    if (u < 0x80) {
        buf[n++] = (char)u;
    } else if (u < 0x800) {
        buf[n++] = (char)(0xC0 | (u >> 6));
        buf[n++] = (char)(0x80 | (u & 0x3F));
    } else if (u < 0x10000) {
        buf[n++] = (char)(0xE0 | (u >> 12));
        buf[n++] = (char)(0x80 | ((u >> 6) & 0x3F));
        buf[n++] = (char)(0x80 | (u & 0x3F));
    } else {
        buf[n++] = (char)(0xF0 | (u >> 18));
        buf[n++] = (char)(0x80 | ((u >> 12) & 0x3F));
        buf[n++] = (char)(0x80 | ((u >> 6) & 0x3F));
        buf[n++] = (char)(0x80 | (u & 0x3F));
    }
    return l_str_new(buf, n);
}

static l_str l_str_from_key(l_key k) {
    switch (k.kind) {
        case L_KEY_STR: return k.s;
        case L_KEY_FLOAT: return l_str_from_float(k.f);
        case L_KEY_BOOL: return l_str_from_bool((int8_t)k.i);
        case L_KEY_CHAR: return l_str_from_char((int32_t)k.i);
        default: return l_str_from_int(k.i);
    }
}

/* ------------------------------------------------------- string methods */

static int64_t l_str_len(l_str s) { return s.len; }

/* Indexing walks UTF-8, so `s[i]` is the i-th character, not the i-th byte. */
static int32_t l_str_index(l_str s, int64_t i) {
    int64_t at = 0, seen = 0;
    while (at < s.len) {
        unsigned char c = (unsigned char)s.data[at];
        int width = c < 0x80 ? 1 : (c < 0xE0 ? 2 : (c < 0xF0 ? 3 : 4));
        if (seen == i) {
            uint32_t u = 0;
            if (width == 1) u = c;
            else if (width == 2) u = ((uint32_t)(c & 0x1F) << 6) | ((unsigned char)s.data[at + 1] & 0x3F);
            else if (width == 3)
                u = ((uint32_t)(c & 0x0F) << 12) | (((unsigned char)s.data[at + 1] & 0x3F) << 6) |
                    ((unsigned char)s.data[at + 2] & 0x3F);
            else
                u = ((uint32_t)(c & 0x07) << 18) | (((unsigned char)s.data[at + 1] & 0x3F) << 12) |
                    (((unsigned char)s.data[at + 2] & 0x3F) << 6) | ((unsigned char)s.data[at + 3] & 0x3F);
            return (int32_t)u;
        }
        at += width;
        seen++;
    }
    l_fail_cstr("string index is out of bounds");
    return 0;
}

static int64_t l_str_chars(l_str s) {
    int64_t at = 0, n = 0;
    while (at < s.len) {
        unsigned char c = (unsigned char)s.data[at];
        at += c < 0x80 ? 1 : (c < 0xE0 ? 2 : (c < 0xF0 ? 3 : 4));
        n++;
    }
    return n;
}

static int8_t l_str_contains(l_str s, l_str needle) {
    if (needle.len == 0) return 1;
    if (needle.len > s.len) return 0;
    for (int64_t i = 0; i + needle.len <= s.len; i++)
        if (memcmp(s.data + i, needle.data, (size_t)needle.len) == 0) return 1;
    return 0;
}

static l_str l_str_trim(l_str s) {
    int64_t a = 0, b = s.len;
    while (a < b && (s.data[a] == ' ' || s.data[a] == '\t' || s.data[a] == '\n' || s.data[a] == '\r')) a++;
    while (b > a && (s.data[b - 1] == ' ' || s.data[b - 1] == '\t' || s.data[b - 1] == '\n' || s.data[b - 1] == '\r')) b--;
    return l_str_new(s.data + a, b - a);
}

static l_str l_str_upper(l_str s) {
    l_str o = l_str_new(s.data, s.len);
    for (int64_t i = 0; i < o.len; i++)
        if (o.data[i] >= 'a' && o.data[i] <= 'z') o.data[i] = (char)(o.data[i] - 32);
    return o;
}

static l_str l_str_lower(l_str s) {
    l_str o = l_str_new(s.data, s.len);
    for (int64_t i = 0; i < o.len; i++)
        if (o.data[i] >= 'A' && o.data[i] <= 'Z') o.data[i] = (char)(o.data[i] + 32);
    return o;
}

static l_str l_str_substr(l_str s, int64_t a, int64_t b) {
    if (a < 0) a = 0;
    if (b > s.len) b = s.len;
    if (a > b) return L_EMPTY_STR;
    return l_str_new(s.data + a, b - a);
}

static l_str l_str_replace(l_str s, l_str from, l_str to) {
    if (from.len == 0) return s;
    l_array out = l_array_new(1, s.len + 1);
    for (int64_t i = 0; i < s.len;) {
        if (i + from.len <= s.len && memcmp(s.data + i, from.data, (size_t)from.len) == 0) {
            for (int64_t j = 0; j < to.len; j++) l_array_push(out, &to.data[j]);
            i += from.len;
        } else {
            l_array_push(out, &s.data[i]);
            i++;
        }
    }
    return l_str_new((char *)out->data, out->len);
}

static l_array l_str_split(l_str s, l_str sep) {
    l_array out = l_array_new((int64_t)sizeof(l_str), 4);
    if (sep.len == 0) {
        l_array_push(out, &s);
        return out;
    }
    int64_t start = 0;
    for (int64_t i = 0; i + sep.len <= s.len;) {
        if (memcmp(s.data + i, sep.data, (size_t)sep.len) == 0) {
            l_str piece = l_str_new(s.data + start, i - start);
            l_array_push(out, &piece);
            i += sep.len;
            start = i;
        } else {
            i++;
        }
    }
    l_str last = l_str_new(s.data + start, s.len - start);
    l_array_push(out, &last);
    return out;
}

static l_str l_str_join(l_array parts, l_str sep) {
    if (!parts || parts->len == 0) return L_EMPTY_STR;
    int64_t n = parts->len * 2 - 1;
    l_str *buf = (l_str *)l_alloc(sizeof(l_str) * (size_t)n);
    int64_t at = 0;
    for (int64_t i = 0; i < parts->len; i++) {
        if (i) buf[at++] = sep;
        buf[at++] = ((l_str *)parts->data)[i];
    }
    return l_str_concat((int)n, buf);
}

static int64_t l_str_to_int(l_str s) {
    char *end = NULL;
    long long v = strtoll(s.data, &end, 10);
    if (end == s.data) {
        l_str parts[3];
        parts[0] = l_str_lit("cannot read an integer from \"");
        parts[1] = s;
        parts[2] = l_str_lit("\"");
        l_fail(l_str_concat(3, parts));
    }
    return (int64_t)v;
}

static double l_str_to_float(l_str s) {
    char *end = NULL;
    double v = strtod(s.data, &end);
    if (end == s.data) {
        l_str parts[3];
        parts[0] = l_str_lit("cannot read a number from \"");
        parts[1] = s;
        parts[2] = l_str_lit("\"");
        l_fail(l_str_concat(3, parts));
    }
    return v;
}

/* ------------------------------------------------------------------ i/o */

static void l_print(l_str s) {
    fwrite(s.data, 1, (size_t)s.len, stdout);
    fputc('\n', stdout);
}

static void l_eprint(l_str s) {
    fwrite(s.data, 1, (size_t)s.len, stderr);
    fputc('\n', stderr);
}

static l_str l_read_line(void) {
    size_t cap = 128, len = 0;
    char *buf = (char *)l_alloc(cap);
    int c;
    while ((c = fgetc(stdin)) != EOF && c != '\n') {
        if (len + 1 >= cap) {
            cap *= 2;
            buf = (char *)l_realloc(buf, cap);
        }
        buf[len++] = (char)c;
    }
    return l_str_new(buf, (int64_t)len);
}

static l_str l_read_file(l_str path) {
    FILE *f = fopen(path.data, "rb");
    if (!f) {
        l_str parts[3];
        parts[0] = l_str_lit("cannot read file \"");
        parts[1] = path;
        parts[2] = l_str_lit("\"");
        l_fail(l_str_concat(3, parts));
    }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    char *buf = (char *)l_alloc((size_t)n + 1);
    size_t got = fread(buf, 1, (size_t)n, f);
    fclose(f);
    return l_str_new(buf, (int64_t)got);
}

static void l_write_file(l_str path, l_str text) {
    FILE *f = fopen(path.data, "wb");
    if (!f) {
        l_str parts[3];
        parts[0] = l_str_lit("cannot write file \"");
        parts[1] = path;
        parts[2] = l_str_lit("\"");
        l_fail(l_str_concat(3, parts));
    }
    fwrite(text.data, 1, (size_t)text.len, f);
    fclose(f);
}

/* ------------------------------------------------------------ process */

static int l_argc = 0;
static char **l_argv = NULL;

static l_array l_args(void) {
    l_array a = l_array_new((int64_t)sizeof(l_str), l_argc > 0 ? l_argc : 1);
    for (int i = 0; i < l_argc; i++) {
        l_str s = l_str_lit(l_argv[i]);
        l_array_push(a, &s);
    }
    return a;
}

static int64_t l_now(void) {
    return (int64_t)(clock() * 1000.0 / CLOCKS_PER_SEC);
}

static void l_assert(int8_t cond, l_str message) {
    if (!cond) l_fail(message);
}

/* Integer division and remainder trap on a zero divisor rather than invoking
 * undefined behaviour. */
static int64_t l_idiv(int64_t a, int64_t b) {
    if (b == 0) l_fail_cstr("division by zero");
    return a / b;
}

static int64_t l_irem(int64_t a, int64_t b) {
    if (b == 0) l_fail_cstr("remainder by zero");
    return a % b;
}
