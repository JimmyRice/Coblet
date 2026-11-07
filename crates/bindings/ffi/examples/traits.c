/*
 * Version trait example of the coblet C ABI, online: a trait table in TOML
 * and a trait source written in C tag versions, a TOML Platform Profile
 * and a customizer act on those traits, and a version is launched until a
 * log message contains the marker.
 *
 * The table adds `acme:offline-skins` to b1.7.3 and removes the built-in
 * `legacy-macos-rendering` from it. The C source adds `acme:from-code` to
 * old betas.
 *
 *   cc -I include examples/traits.c -L <target dir> -lcoblet_ffi -o traits
 *   ./traits <data root> <version> <marker>
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "coblet.h"

static const char *TABLE =
    "id = \"acme:fixes\"\n"
    "name = \"Acme fixes\"\n"
    "[[rule]]\n"
    "add = [\"acme:offline-skins\"]\n"
    "ids = [\"b1.7.3\"]\n"
    "[[rule]]\n"
    "remove = [\"legacy-macos-rendering\"]\n"
    "ids = [\"b1.7.3\"]\n";

static const char *PROFILE =
    "id = \"acme:offline-skins\"\n"
    "[match]\n"
    "version_traits = [\"acme:offline-skins\"]\n"
    "[jvm]\n"
    "append = [\"-Dacme.traits=ok\"]\n";

typedef struct {
    char *ptr;
    size_t len;
    size_t cap;
} Buf;

static void put(Buf *buf, const char *text, size_t len) {
    if (buf->len + len + 1 > buf->cap) {
        buf->cap = (buf->len + len + 1) * 2;
        buf->ptr = realloc(buf->ptr, buf->cap);
        if (buf->ptr == NULL) {
            abort();
        }
    }
    memcpy(buf->ptr + buf->len, text, len);
    buf->len += len;
    buf->ptr[buf->len] = '\0';
}

/* Returns the end of the JSON string whose opening quote is at `s`. */
static const char *skip_string(const char *s) {
    for (s++; *s != '\0' && *s != '"'; s++) {
        if (*s == '\\' && s[1] != '\0') {
            s++;
        }
    }
    return *s != '\0' ? s + 1 : s;
}

/*
 * Adds `acme:from-code` to old betas and keeps the other traits. A real
 * source would use a JSON library; this scanner reads only the `type` and
 * `traits` keys of each version, which sit three levels deep.
 */
static bool old_betas(void *user_data, CobletTraitCtx *ctx, const char *batch) {
    unsigned *batches = user_data;
    (*batches)++;
    Buf out = {0};
    put(&out, "[", 1);
    int depth = 0;
    bool first = true;
    bool old_beta = false;
    const char *key = "";
    size_t key_len = 0;
    const char *traits = NULL;
    size_t traits_len = 0;
    for (const char *p = batch; *p != '\0'; p++) {
        if (*p == '"') {
            const char *end = skip_string(p);
            if (depth == 3 && *end == ':') {
                key = p + 1;
                key_len = (size_t)(end - p - 2);
            } else if (depth == 3 && key_len == 4 && strncmp(key, "type", 4) == 0) {
                old_beta = end - p == 10 && strncmp(p, "\"old_beta\"", 10) == 0;
            }
            p = end - 1;
        } else if (*p == '{' || *p == '[') {
            if (*p == '[' && depth == 3 && key_len == 6 && strncmp(key, "traits", 6) == 0) {
                traits = p;
                traits_len = (size_t)(strchr(p, ']') - p + 1);
            }
            if (++depth == 3) {
                old_beta = false;
            }
        } else if (*p == '}' || *p == ']') {
            if (--depth == 2 && *p == '}' && traits != NULL) {
                if (!first) {
                    put(&out, ",", 1);
                }
                first = false;
                if (!old_beta) {
                    put(&out, traits, traits_len);
                } else if (traits_len == 2) {
                    put(&out, "[\"acme:from-code\"]", 18);
                } else {
                    put(&out, traits, traits_len - 1);
                    put(&out, ",\"acme:from-code\"]", 18);
                }
            }
        }
    }
    put(&out, "]", 1);
    bool ok = coblet_trait_ctx_set_result(ctx, out.ptr) == COBLET_STATUS_OK;
    free(out.ptr);
    return ok;
}

static bool tagged_by_code(void *user_data, const char *traits_json) {
    (void)user_data;
    return strstr(traits_json, "\"acme:from-code\"") != NULL;
}

static bool add_marker(void *user_data, CobletProfileEdit *edit) {
    (void)user_data;
    return coblet_edit_add_arg(edit, COBLET_ARG_KIND_JVM, "-Dacme.code=ok", false, NULL) ==
           COBLET_STATUS_OK;
}

static int fail(const char *what, CobletError *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err->message.ptr : "(no details)");
    coblet_error_free(err);
    return 1;
}

/* Prints the lines of `text` that contain `needle`, each after `label`. */
static void print_lines(const char *label, const char *text, const char *needle) {
    const char *line = text;
    while (*line != '\0') {
        const char *end = strchr(line, '\n');
        size_t len = end ? (size_t)(end - line) : strlen(line);
        char *copy = strndup(line, len);
        if (copy != NULL && strstr(copy, needle) != NULL) {
            printf("%s: %s\n", label, copy);
        }
        free(copy);
        line += len + (end ? 1 : 0);
    }
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s <data root> <version> <marker>\n", argv[0]);
        return 2;
    }
    printf("abi version %u\n", coblet_abi_version());

    CobletError *err = NULL;
    CobletBuilder *builder = NULL;
    if (coblet_builder_new(argv[1], "CobletTraitExample/1.0 (example@example.com)", &builder,
                           &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_new", err);
    }
    if (coblet_builder_add_trait_table_toml(builder, TABLE, 0, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_trait_table_toml", err);
    }
    static unsigned batches = 0;
    CobletTraitSource source = {&batches, "acme:from-code", "Acme traits from code", old_betas,
                                NULL};
    if (coblet_builder_add_trait_source(builder, source, 0, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_trait_source", err);
    }
    if (coblet_builder_add_platform_profile_toml(builder, PROFILE, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_platform_profile_toml", err);
    }
    CobletCustomizer customizer = {"acme:marker", "Acme marker", NULL, tagged_by_code, add_marker,
                                   NULL};
    if (coblet_builder_add_customizer(builder, customizer, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_customizer", err);
    }
    Coblet *coblet = NULL;
    if (coblet_builder_build(builder, &coblet, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_build", err);
    }

    CobletString json = {0};
    if (coblet_trait_sources_json(coblet, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_trait_sources_json", err);
    }
    printf("trait sources: %s\n", json.ptr);
    coblet_string_free(json);
    if (coblet_versions_json(coblet, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_versions_json", err);
    }
    printf("listing batches: %u\n", batches);
    coblet_string_free(json);
    if (coblet_version_traits_json(coblet, argv[2], &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_version_traits_json", err);
    }
    printf("%s at launch: %s\n", argv[2], json.ptr);
    coblet_string_free(json);

    const char *customizers[] = {"acme:marker"};
    CobletGame *game = NULL;
    if (coblet_launch_customized(coblet, argv[2], "Steve", COBLET_LAUNCH_MODE_OFFLINE, NULL,
                                 customizers, 1, NULL, &game, &err) != COBLET_STATUS_OK) {
        return fail("coblet_launch_customized", err);
    }
    CobletString report = {0};
    if (coblet_game_report(game, &report, &err) != COBLET_STATUS_OK) {
        return fail("coblet_game_report", err);
    }
    print_lines("report", report.ptr, "acme");
    coblet_string_free(report);

    int reached = 0;
    CobletString line = {0};
    CobletStatus status;
    while ((status = coblet_game_next_log(game, &line, &err)) == COBLET_STATUS_OK) {
        if (!reached && strstr(line.ptr, argv[3]) != NULL) {
            printf("reached marker: %s\n", line.ptr);
            reached = 1;
            coblet_game_kill(game);
        }
        coblet_string_free(line);
    }
    if (status != COBLET_STATUS_END) {
        return fail("coblet_game_next_log", err);
    }
    CobletGameExit exit_info = {0};
    if (coblet_game_wait(game, &exit_info, &err) != COBLET_STATUS_OK) {
        return fail("coblet_game_wait", err);
    }
    printf("exit: killed %d, %llu ms, %u batches in all\n", exit_info.killed,
           (unsigned long long)exit_info.duration_ms, batches);
    coblet_game_free(game);
    coblet_free(coblet);
    return reached ? 0 : 1;
}
