/*
 * Signs in to Microsoft through the coblet C ABI with a device code, keeps
 * the tokens in an encrypted file, and launches a version online with the
 * account until a log message contains a marker.
 *
 *   cc -I include examples/accounts.c -L <target dir> -lcoblet_ffi -o accounts
 *   COBLET_MSA_CLIENT_ID=<your client id> ./accounts <data root> <version> <marker>
 *
 * The key is generated for this run only, so the next run signs in again;
 * a real launcher keeps the key in a system keychain.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "coblet.h"

static int fail(const char *what, CobletError *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err->message.ptr : "(no details)");
    coblet_error_free(err);
    return 1;
}

/* Copies the string value of `"key":"..."` out of a flat JSON document. */
static int json_string(const char *json, const char *key, char *out, size_t size) {
    char pattern[64];
    snprintf(pattern, sizeof pattern, "\"%s\":\"", key);
    const char *start = strstr(json, pattern);
    if (start == NULL) {
        return 0;
    }
    start += strlen(pattern);
    const char *end = strchr(start, '"');
    size_t len = end ? (size_t)(end - start) : 0;
    if (end == NULL || len >= size) {
        return 0;
    }
    memcpy(out, start, len);
    out[len] = '\0';
    return 1;
}

int main(int argc, char **argv) {
    const char *client_id = getenv("COBLET_MSA_CLIENT_ID");
    if (argc != 4 || client_id == NULL) {
        fprintf(stderr, "usage: COBLET_MSA_CLIENT_ID=<id> %s <data root> <version> <marker>\n",
                argv[0]);
        return 2;
    }
    CobletError *err = NULL;
    CobletBuilder *builder = NULL;
    if (coblet_builder_new(argv[1], "coblet-ffi-accounts/0.1", &builder, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_new", err);
    }
    uint8_t key[COBLET_SECRET_KEY_LEN];
    char path[4096];
    snprintf(path, sizeof path, "%s/secrets.bin", argv[1]);
    if (coblet_builder_set_msa_client_id(builder, client_id, &err) != COBLET_STATUS_OK ||
        coblet_generate_secret_key(key, sizeof key, &err) != COBLET_STATUS_OK ||
        coblet_builder_set_encrypted_secret_file(builder, path, key, sizeof key, &err) !=
            COBLET_STATUS_OK) {
        coblet_builder_free(builder);
        return fail("builder setup", err);
    }
    Coblet *coblet = NULL;
    if (coblet_builder_build(builder, &coblet, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_build", err);
    }

    CobletLogin *login = NULL;
    if (coblet_login_begin(coblet, COBLET_LOGIN_INTERACTION_DEVICE_CODE, NULL, NULL, &login,
                           &err) != COBLET_STATUS_OK) {
        return fail("coblet_login_begin", err);
    }
    CobletString prompt = {0};
    if (coblet_login_prompt_json(login, &prompt, &err) != COBLET_STATUS_OK) {
        return fail("coblet_login_prompt_json", err);
    }
    char code[64], uri[256];
    if (json_string(prompt.ptr, "user_code", code, sizeof code) &&
        json_string(prompt.ptr, "verification_uri", uri, sizeof uri)) {
        printf("open %s and enter %s\n", uri, code);
    } else {
        printf("prompt: %s\n", prompt.ptr);
    }
    coblet_string_free(prompt);
    fflush(stdout);

    CobletString account = {0};
    if (coblet_login_finish(login, &account, &err) != COBLET_STATUS_OK) {
        return fail("coblet_login_finish", err);
    }
    coblet_login_free(login);
    printf("signed in: %s\n", account.ptr);
    char id[80];
    if (!json_string(account.ptr, "id", id, sizeof id)) {
        return fail("reading the account id", NULL);
    }
    coblet_string_free(account);

    CobletGame *game = NULL;
    if (coblet_launch_account(coblet, argv[2], id, NULL, NULL, 0, NULL, &game, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_launch_account", err);
    }
    int reached = 0;
    CobletString line = {0};
    CobletStatus status;
    while ((status = coblet_game_next_log(game, &line, &err)) == COBLET_STATUS_OK) {
        printf("%s\n", line.ptr);
        if (!reached && strstr(line.ptr, argv[3]) != NULL) {
            printf("reached marker, killing the game\n");
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
    printf("exit: killed %d, suspected crash %d\n", exit_info.killed, exit_info.suspected_crash);
    coblet_game_free(game);
    coblet_free(coblet);
    return reached ? 0 : 1;
}
