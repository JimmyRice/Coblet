/*
 * Mod Loader example of the coblet C ABI: a loader written in C and one
 * described in TOML list their versions, give an install recipe and are
 * resolved on top of 1.20.1.
 *
 * Mirror rules with fallback off point Mojang's metadata server and the
 * loaders' metadata server at <base>, so the example runs against a local
 * server; the C loader's requests go through the same rules as built-in
 * ones. <base> serves mojang/... as piston-meta.mojang.com and
 * meta/versions/1.20.1 as the loader listing, for example:
 *
 *   site=target/loader-site; m=$site/mojang
 *   mkdir -p $m/mc/game $m/v1/packages/72ae93b0934df5541860e3e6b72cf883c74da491 $site/meta/versions
 *   cp fixtures/mojang/version_manifest_v2.json $m/mc/game/
 *   cp fixtures/mojang/1.20.1.json $m/v1/packages/72ae93b0934df5541860e3e6b72cf883c74da491/
 *   echo '[{"id":"2.0","type":"beta","stable":false},{"id":"1.4.2","recommended":true}]' > $site/meta/versions/1.20.1
 *   python3 -m http.server -d $site 8765 &
 *
 *   cc -I include examples/loaders.c -L <target dir> -lcoblet_ffi -o loaders
 *   ./loaders <data root> http://127.0.0.1:8765
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "coblet.h"

static const char *DATA_LOADER =
    "id = \"example:data-loader\"\n"
    "name = \"Example Data Loader\"\n"
    "uid = \"example.data.loader\"\n"
    "[versions]\n"
    "format = \"json\"\n"
    "url = \"https://meta.example.test/versions/{game}\"\n"
    "id = \"/id\"\n"
    "stable = \"/stable\"\n"
    "recommended = \"/recommended\"\n"
    "[profile]\n"
    "format = \"static\"\n"
    "[[profile.components]]\n"
    "uid = \"example.data.loader\"\n"
    "version = \"{version}\"\n"
    "requires = [{ uid = \"net.minecraft\", equals = \"{game}\" }]\n"
    "mainClass = \"example.data.Main\"\n"
    "libraries = [{ name = \"example.data:loader:{version}\", url = \"https://maven.example.test/\" }]\n";

/* The listing is already in the loader format, so it is handed on as is. */
static bool c_versions(void *user_data, CobletLoaderCtx *ctx, const char *game) {
    (void)user_data;
    char url[256], name[256];
    snprintf(url, sizeof url, "https://meta.example.test/versions/%s", game);
    snprintf(name, sizeof name, "versions/%s.json", game);
    CobletBytes body = {0};
    CobletError *err = NULL;
    if (coblet_loader_ctx_fetch(ctx, url, name, &body, &err) != COBLET_STATUS_OK) {
        coblet_loader_ctx_fail(ctx, err ? err->message.ptr : "fetch failed");
        coblet_error_free(err);
        return false;
    }
    char *text = malloc(body.len + 1);
    memcpy(text, body.ptr, body.len);
    text[body.len] = '\0';
    coblet_bytes_free(body);
    bool ok = coblet_loader_ctx_set_result(ctx, text) == COBLET_STATUS_OK;
    free(text);
    return ok;
}

/* Reads the version ID with a naive search; a real loader would use a JSON parser. */
static bool c_install(void *user_data, CobletLoaderCtx *ctx, const char *game,
                      const char *version_json) {
    (void)user_data;
    const char *start = strstr(version_json, "\"id\":\"");
    if (start == NULL) {
        coblet_loader_ctx_fail(ctx, "no version id");
        return false;
    }
    start += 6;
    int len = (int)(strchr(start, '"') - start);
    char recipe[1024];
    snprintf(recipe, sizeof recipe,
             "{\"components\":[{\"uid\":\"example.c.loader\",\"version\":\"%.*s\","
             "\"requires\":[{\"uid\":\"net.minecraft\",\"equals\":\"%s\"}],"
             "\"mainClass\":\"example.c.Main\",\"+tweakers\":[\"example.c.Tweaker\"],"
             "\"libraries\":[{\"name\":\"example.c:loader:%.*s\","
             "\"url\":\"https://maven.example.test/\"}]}]}",
             len, start, game, len, start);
    return coblet_loader_ctx_set_result(ctx, recipe) == COBLET_STATUS_OK;
}

static bool check(CobletStatus status, CobletError *err, const char *what) {
    if (status == COBLET_STATUS_OK) {
        return true;
    }
    fprintf(stderr, "%s failed: %s\n", what, err ? err->message.ptr : "(no error)");
    coblet_error_free(err);
    return false;
}

static int show(Coblet *sdk, const char *loader) {
    CobletString out = {0};
    CobletError *err = NULL;
    if (!check(coblet_loader_versions_json(sdk, loader, "1.20.1", &out, &err), err, "versions")) {
        return 1;
    }
    printf("%s versions: %s\n", loader, out.ptr);
    coblet_string_free(out);
    if (!check(coblet_loader_recipe_json(sdk, loader, "1.20.1", "1.4.2", &out, &err), err,
               "recipe")) {
        return 1;
    }
    printf("  recipe: %s\n", out.ptr);
    coblet_string_free(out);
    if (!check(coblet_plan_launch_json(sdk, "1.20.1", loader, "1.4.2", &out, &err), err,
               "plan")) {
        return 1;
    }
    const char *main_class = strstr(out.ptr, "\"main_class\"");
    printf("  plan: %.60s...\n", main_class ? main_class : out.ptr);
    coblet_string_free(out);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s <data root> <base>\n", argv[0]);
        return 2;
    }
    printf("abi version %u\n", coblet_abi_version());
    char mojang[512], meta[512];
    snprintf(mojang, sizeof mojang, "%s/mojang/", argv[2]);
    snprintf(meta, sizeof meta, "%s/meta/", argv[2]);

    CobletBuilder *builder = NULL;
    CobletError *err = NULL;
    if (!check(coblet_builder_new(argv[1], "CobletLoaderExample/1.0 (example@example.com)",
                                  &builder, &err),
               err, "builder")) {
        return 1;
    }
    CobletModLoader loader = {
        .user_data = NULL,
        .id = "example:c-loader",
        .name = "Example C Loader",
        .component_uid = "example.c.loader",
        .versions = c_versions,
        .install = c_install,
        .free = NULL,
    };
    bool ok =
        check(coblet_builder_add_mirror(builder, "https://piston-meta.mojang.com/", mojang, NULL,
                                        0, &err),
              err, "mirror") &&
        check(coblet_builder_add_mirror(builder, "https://meta.example.test/", meta,
                                        "[\"loader\"]", 0, &err),
              err, "loader mirror") &&
        check(coblet_builder_set_mirror_fallback(builder, false, &err), err, "fallback") &&
        check(coblet_builder_add_loader(builder, loader, 0, &err), err, "loader") &&
        check(coblet_builder_add_loader_toml(builder, DATA_LOADER, 0, &err), err, "data loader");
    if (!ok) {
        coblet_builder_free(builder);
        return 1;
    }
    Coblet *sdk = NULL;
    if (!check(coblet_builder_build(builder, &sdk, &err), err, "build")) {
        return 1;
    }

    CobletString out = {0};
    if (check(coblet_loaders_json(sdk, &out, &err), err, "loaders")) {
        printf("loaders: %s\n", out.ptr);
        coblet_string_free(out);
    }
    int status = show(sdk, "example:c-loader") || show(sdk, "example:data-loader");

    if (coblet_loader_versions_json(sdk, "example:missing", "1.20.1", &out, &err) !=
        COBLET_STATUS_OK) {
        printf("unknown loader: kind %d, %s\n", (int)err->kind, err->message.ptr);
        coblet_error_free(err);
    }
    coblet_free(sdk);
    return status;
}
