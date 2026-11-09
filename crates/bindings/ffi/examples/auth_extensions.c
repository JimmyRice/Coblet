/*
 * Auth extensions from C: an auth method written in C that signs players
 * in by name and launches them offline with an extra JVM argument, and an
 * appearance service written in C that keeps their skin in memory.
 * Signs in, uploads and resets a skin, shows that changes the service
 * cannot make are refused, then launches the account and stops the game
 * once a log message contains a marker.
 *
 *   cc -I include examples/auth_extensions.c -L <target dir> -lcoblet_ffi -o auth_extensions
 *   ./auth_extensions <data root> <version> <marker>
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "coblet.h"

#define UUID "0f5c5f6b8a7d4e2fb1d3a8c9e0f1a2b3"

/* Skin kept by the appearance service; a real one would talk to a site. */
typedef struct {
    size_t bytes;
    char model[8];
} Wardrobe;

/*
 * Copies the string value of `"key":"..."` from a JSON document into `out`.
 * Enough for the flat documents here; a real program would use a JSON
 * library.
 */
static bool json_text(const char *json, const char *key, char *out, size_t size) {
    char pattern[64];
    snprintf(pattern, sizeof pattern, "\"%s\":\"", key);
    const char *start = strstr(json, pattern);
    if (start == NULL) {
        return false;
    }
    start += strlen(pattern);
    const char *end = strchr(start, '"');
    if (end == NULL || (size_t)(end - start) >= size) {
        return false;
    }
    memcpy(out, start, (size_t)(end - start));
    out[end - start] = '\0';
    return true;
}

/* Signs in with the `name` param; no user interaction, so no prompt. */
static bool login(void *user_data, CobletAuthCtx *ctx, const char *request) {
    (void)user_data;
    char name[32];
    if (!json_text(request, "name", name, sizeof name)) {
        coblet_auth_ctx_fail(ctx, COBLET_ERROR_KIND_INVALID_ARGUMENT, "the name param is missing");
        return false;
    }
    char change[256];
    snprintf(change, sizeof change,
             "{\"player\":{\"name\":\"%s\",\"uuid\":\"" UUID "\"},\"state\":\"ready\","
             "\"data\":{\"name\":\"%s\"}}",
             name, name);
    return coblet_auth_ctx_set_result(ctx, change) == COBLET_STATUS_OK;
}

/* Launches offline as the saved name, with a JVM argument of the method. */
static bool refresh(void *user_data, CobletAuthCtx *ctx, const char *request) {
    (void)user_data;
    char name[32];
    if (!json_text(request, "name", name, sizeof name)) {
        coblet_auth_ctx_fail(ctx, COBLET_ERROR_KIND_AUTH, "sign in again");
        return false;
    }
    char answer[512];
    snprintf(answer, sizeof answer,
             "{\"change\":{\"player\":{\"name\":\"%s\",\"uuid\":\"" UUID "\"},"
             "\"state\":\"ready\",\"data\":{\"name\":\"%s\"}},"
             "\"session\":{\"player_name\":\"%s\",\"access_token\":null,"
             "\"jvm_args\":[\"-Dacme.auth=c\"]}}",
             name, name, name);
    return coblet_auth_ctx_set_result(ctx, answer) == COBLET_STATUS_OK;
}

/* Answers with the player and the skin it wears, if any. */
static bool wear(CobletAppearanceCtx *ctx, const Wardrobe *wardrobe, const char *request) {
    char name[32] = "Steve";
    json_text(request, "name", name, sizeof name);
    char player[512];
    if (wardrobe->bytes == 0) {
        snprintf(player, sizeof player, "{\"name\":\"%s\",\"uuid\":\"" UUID "\"}", name);
    } else {
        snprintf(player, sizeof player,
                 "{\"name\":\"%s\",\"uuid\":\"" UUID "\",\"skins\":[{\"id\":\"skin-%zu\","
                 "\"url\":\"https://skins.example/skin.png\",\"model\":\"%s\",\"active\":true}]}",
                 name, wardrobe->bytes, wardrobe->model);
    }
    return coblet_appearance_ctx_set_result(ctx, player) == COBLET_STATUS_OK;
}

static bool appearance(void *user_data, CobletAppearanceCtx *ctx, const char *request) {
    Wardrobe *wardrobe = user_data;
    char op[32];
    json_text(request, "op", op, sizeof op);
    if (strcmp(op, "upload_skin_png") == 0) {
        const uint8_t *png = NULL;
        size_t len = 0;
        if (coblet_appearance_ctx_png(ctx, &png, &len, NULL) != COBLET_STATUS_OK) {
            return false;
        }
        wardrobe->bytes = len;
        json_text(request, "model", wardrobe->model, sizeof wardrobe->model);
    } else if (strcmp(op, "reset_skin") == 0) {
        wardrobe->bytes = 0;
    } else if (strcmp(op, "appearance") != 0) {
        coblet_appearance_ctx_fail(ctx, COBLET_ERROR_KIND_UNSUPPORTED, "not supported");
        return false;
    }
    return wear(ctx, wardrobe, request);
}

static void release(void *user_data) {
    free(user_data);
}

static int fail(const char *what, CobletError *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err->message.ptr : "(no details)");
    coblet_error_free(err);
    return 1;
}

/* Prints a JSON answer and frees it. */
static void show(const char *what, CobletString json) {
    printf("%s: %s\n", what, json.ptr);
    coblet_string_free(json);
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s <data root> <version> <marker>\n", argv[0]);
        return 2;
    }
    CobletError *err = NULL;
    CobletBuilder *builder = NULL;
    if (coblet_builder_new(argv[1], "coblet-ffi-auth-extensions/0.1", &builder, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_builder_new", err);
    }
    CobletAuthMethod method = {NULL, "acme:names", "Acme names", "acme:wardrobe",
                               login, refresh, NULL};
    if (coblet_builder_add_auth_method(builder, method, 0, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_auth_method", err);
    }
    Wardrobe *wardrobe = calloc(1, sizeof *wardrobe);
    CobletAppearanceService service = {
        wardrobe, "acme:wardrobe", "Acme wardrobe",
        COBLET_APPEARANCE_CAN_UPLOAD_PNG | COBLET_APPEARANCE_CAN_RESET_SKIN,
        appearance, release};
    if (coblet_builder_add_appearance_service(builder, service, 0, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_appearance_service", err);
    }
    Coblet *coblet = NULL;
    if (coblet_builder_build(builder, &coblet, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_build", err);
    }
    CobletString json = {0};
    if (coblet_auth_methods_json(coblet, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_auth_methods_json", err);
    }
    show("auth methods", json);

    CobletLogin *login_handle = NULL;
    if (coblet_login_begin_with(coblet, "acme:names", "{\"params\":{\"name\":\"Alex\"}}",
                                &login_handle, &err) != COBLET_STATUS_OK) {
        return fail("coblet_login_begin_with", err);
    }
    if (coblet_login_finish(login_handle, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_login_finish", err);
    }
    coblet_login_free(login_handle);
    char account[64];
    if (!json_text(json.ptr, "id", account, sizeof account)) {
        fprintf(stderr, "the account has no id\n");
        return 1;
    }
    show("signed in", json);

    if (coblet_appearance_capabilities_json(coblet, account, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_appearance_capabilities_json", err);
    }
    show("capabilities", json);
    uint8_t png[33] = {0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n', 0, 0, 0, 13, 'I', 'H', 'D', 'R',
                       0, 0, 0, 64, 0, 0, 0, 64, 8, 6, 0, 0, 0};
    if (coblet_account_upload_skin(coblet, account, png, sizeof png, COBLET_SKIN_MODEL_SLIM, &json,
                                   &err) != COBLET_STATUS_OK) {
        return fail("coblet_account_upload_skin", err);
    }
    show("uploaded", json);
    if (coblet_account_reset_skin(coblet, account, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_account_reset_skin", err);
    }
    show("reset", json);
    if (coblet_account_set_cape(coblet, account, "migrator", &json, &err) == COBLET_STATUS_OK) {
        fprintf(stderr, "the cape change should have been refused\n");
        return 1;
    }
    printf("set cape refused: kind %d\n", err->kind);
    coblet_error_free(err);
    err = NULL;

    CobletGame *game = NULL;
    if (coblet_launch_account(coblet, argv[2], account, NULL, NULL, 0, NULL, &game, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_launch_account", err);
    }
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
    printf("exit: killed %d\n", exit_info.killed);
    coblet_game_free(game);
    coblet_free(coblet);
    return reached ? 0 : 1;
}
