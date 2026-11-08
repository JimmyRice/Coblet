/*
 * Version source example of the coblet C ABI, online: Mojang's manifest is
 * removed and a game version source written in C lists only the versions
 * it pins, read from Mojang's manifest through the source context. The
 * first pinned version is launched until a log message contains the
 * marker.
 *
 * A Mod Loader described in TOML also gets versions from a static source
 * in TOML and from one written in C; the merged list shows who listed
 * each. That part makes no requests.
 *
 *   cc -I include examples/sources.c -L <target dir> -lcoblet_ffi -o sources
 *   ./sources <data root> <marker> <version>...
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "coblet.h"

static const char *MANIFEST = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

static const char *LOADER =
    "id = \"example:loader\"\n"
    "name = \"Example Loader\"\n"
    "uid = \"example.loader\"\n"
    "[versions]\n"
    "format = \"static\"\n"
    "list = [{ id = \"1.0\", recommended = true }]\n"
    "[profile]\n"
    "format = \"static\"\n"
    "[[profile.components]]\n"
    "uid = \"example.loader\"\n"
    "version = \"{version}\"\n";

static const char *MIRROR =
    "id = \"example:mirror\"\n"
    "name = \"Example mirror\"\n"
    "[versions]\n"
    "format = \"static\"\n"
    "list = [{ id = \"1.0\" }, { id = \"0.9\", type = \"beta\" }]\n";

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

static void puts_buf(Buf *buf, const char *text) { put(buf, text, strlen(text)); }

/*
 * Copies the string value of `key` from the manifest entry between `from`
 * and `to`, quotes included, into `out`. A real source would use a JSON
 * library; Mojang's entries are flat objects of strings and numbers.
 */
static bool copy_field(Buf *out, const char *from, const char *to, const char *key) {
    char pattern[32];
    snprintf(pattern, sizeof pattern, "\"%s\":", key);
    const char *at = strstr(from, pattern);
    if (at == NULL || at > to) {
        return false;
    }
    at = strchr(at + strlen(pattern), '"');
    const char *end = at ? strchr(at + 1, '"') : NULL;
    if (end == NULL || end > to) {
        return false;
    }
    put(out, at, (size_t)(end - at + 1));
    return true;
}

typedef struct {
    char **ids;
    int count;
    unsigned calls;
} Pins;

/* Lists the pinned versions with the URL and hash Mojang gives them. */
static bool pinned_versions(void *user_data, CobletSourceCtx *ctx, const char *query) {
    (void)query;
    Pins *pins = user_data;
    pins->calls++;
    CobletBytes body = {0};
    CobletError *err = NULL;
    if (coblet_source_ctx_fetch(ctx, MANIFEST, "mojang-manifest.json", &body, &err) !=
        COBLET_STATUS_OK) {
        coblet_source_ctx_fail(ctx, err ? err->message.ptr : "fetch failed");
        coblet_error_free(err);
        return false;
    }
    char *manifest = strndup((const char *)body.ptr, body.len);
    coblet_bytes_free(body);
    if (manifest == NULL) {
        return false;
    }
    Buf out = {0};
    puts_buf(&out, "[");
    bool ok = true;
    for (int i = 0; i < pins->count && ok; i++) {
        char needle[64];
        snprintf(needle, sizeof needle, "\"id\": \"%s\"", pins->ids[i]);
        const char *entry = strstr(manifest, needle);
        const char *end = entry ? strchr(entry, '}') : NULL;
        if (end == NULL) {
            char message[96];
            snprintf(message, sizeof message, "mojang does not list %s", pins->ids[i]);
            coblet_source_ctx_fail(ctx, message);
            ok = false;
            break;
        }
        puts_buf(&out, i == 0 ? "{\"latest_release\":true" : ",{\"latest_release\":false");
        puts_buf(&out, ",\"traits\":[\"example:pinned\"]");
        const char *keys[][2] = {
            {"id", "id"}, {"type", "type"}, {"url", "url"}, {"sha1", "sha1"},
            {"releaseTime", "released"}};
        for (size_t k = 0; k < sizeof keys / sizeof keys[0] && ok; k++) {
            puts_buf(&out, ",\"");
            puts_buf(&out, keys[k][1]);
            puts_buf(&out, "\":");
            ok = copy_field(&out, entry, end, keys[k][0]);
        }
        puts_buf(&out, "}");
    }
    puts_buf(&out, "]");
    if (ok) {
        ok = coblet_source_ctx_set_result(ctx, out.ptr) == COBLET_STATUS_OK;
    }
    free(out.ptr);
    free(manifest);
    return ok;
}

/* Lists a nightly build of the example loader for every game version. */
static bool nightly(void *user_data, CobletSourceCtx *ctx, const char *query) {
    (void)user_data;
    printf("nightly query: %s\n", query);
    return coblet_source_ctx_set_result(ctx, "[{\"id\":\"2.0-nightly\",\"type\":\"beta\"}]") ==
           COBLET_STATUS_OK;
}

static int fail(const char *what, CobletError *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err->message.ptr : "(no details)");
    coblet_error_free(err);
    return 1;
}

/* Prints `json` after `label` and fails unless it contains every needle. */
static int expect(const char *label, const char *json, const char *const *needles) {
    printf("%s: %s\n", label, json);
    for (; *needles != NULL; needles++) {
        if (strstr(json, *needles) == NULL) {
            fprintf(stderr, "%s lacks %s\n", label, *needles);
            return 1;
        }
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: %s <data root> <marker> <version>...\n", argv[0]);
        return 2;
    }
    const char *version = argv[3];
    printf("abi version %u\n", coblet_abi_version());

    CobletError *err = NULL;
    CobletBuilder *builder = NULL;
    if (coblet_builder_new(argv[1], "CobletSourceExample/1.0 (example@example.com)", &builder,
                           &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_new", err);
    }
    if (coblet_builder_remove_version_source(builder, "coblet:mojang", &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_builder_remove_version_source", err);
    }
    static Pins pins;
    pins.ids = argv + 3;
    pins.count = argc - 3;
    CobletVersionSource pinned = {&pins, "example:pinned", "Pinned versions", pinned_versions,
                                  NULL};
    if (coblet_builder_add_version_source(builder, pinned, 0, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_version_source", err);
    }
    if (coblet_builder_add_loader_toml(builder, LOADER, 0, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_loader_toml", err);
    }
    if (coblet_builder_add_loader_version_source_toml(builder, "example:loader", MIRROR, 0,
                                                      &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_loader_version_source_toml", err);
    }
    CobletVersionSource builds = {NULL, "example:nightly", "Nightly builds", nightly, NULL};
    if (coblet_builder_add_loader_version_source(builder, "example:loader", builds, 0, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_builder_add_loader_version_source", err);
    }
    Coblet *coblet = NULL;
    if (coblet_builder_build(builder, &coblet, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_build", err);
    }

    CobletString json = {0};
    int bad = 0;
    if (coblet_version_sources_json(coblet, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_version_sources_json", err);
    }
    const char *game_sources[] = {"\"example:pinned\"", NULL};
    bad |= expect("version sources", json.ptr, game_sources);
    bad |= strstr(json.ptr, "coblet:mojang") != NULL;
    coblet_string_free(json);
    if (coblet_versions_json(coblet, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_versions_json", err);
    }
    const char *listed[] = {"\"source\":\"example:pinned\"", NULL};
    bad |= expect("versions", json.ptr, listed);
    coblet_string_free(json);
    if (coblet_loader_version_sources_json(coblet, "example:loader", &json, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_loader_version_sources_json", err);
    }
    printf("loader sources: %s\n", json.ptr);
    coblet_string_free(json);
    if (coblet_loader_versions_json(coblet, "example:loader", version, &json, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_loader_versions_json", err);
    }
    const char *merged[] = {"\"source\":\"example:loader\"", "\"source\":\"example:mirror\"",
                            "\"source\":\"example:nightly\"", NULL};
    bad |= expect("loader versions", json.ptr, merged);
    coblet_string_free(json);
    if (bad) {
        coblet_free(coblet);
        return 1;
    }

    CobletGame *game = NULL;
    if (coblet_launch(coblet, version, "Steve", COBLET_LAUNCH_MODE_OFFLINE, NULL, &game, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_launch", err);
    }
    int reached = 0;
    CobletString line = {0};
    CobletStatus status;
    while ((status = coblet_game_next_log(game, &line, &err)) == COBLET_STATUS_OK) {
        if (!reached && strstr(line.ptr, argv[2]) != NULL) {
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
    printf("exit: killed %d, %llu ms, %u listings\n", exit_info.killed,
           (unsigned long long)exit_info.duration_ms, pins.calls);
    coblet_game_free(game);
    coblet_free(coblet);
    return reached ? 0 : 1;
}
