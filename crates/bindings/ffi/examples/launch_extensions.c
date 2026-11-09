/*
 * Launch extensions from C: replace the `client-jar` step with one that
 * runs the game from a copy of the client jar, remove `logging-config`,
 * fill `${name}` placeholders from a static value, a step and a
 * placeholder source, and let a customizer use them in JVM arguments.
 * Prints the step list and the placeholder lines of the launch report
 * (names and origins, never values), then stops the game once a log
 * message contains a marker.
 *
 *   cc -I include examples/launch_extensions.c -L <target dir> -lcoblet_ffi -o launch_extensions
 *   ./launch_extensions <data root> <version> <marker>
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "coblet.h"

/* What the steps need to know; a real launcher would read it from its settings. */
typedef struct {
    const char *data_root;
    const char *version;
} Launch;

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

static bool copy_file(const char *from, const char *to) {
    FILE *in = fopen(from, "rb");
    if (in == NULL) {
        return false;
    }
    FILE *out = fopen(to, "wb");
    if (out == NULL) {
        fclose(in);
        return false;
    }
    char buffer[65536];
    size_t read;
    bool ok = true;
    while ((read = fread(buffer, 1, sizeof buffer, in)) > 0) {
        ok = ok && fwrite(buffer, 1, read, out) == read;
    }
    ok = ok && !ferror(in);
    fclose(in);
    return fclose(out) == 0 && ok;
}

/*
 * Stands in for `client-jar`: runs the game from a copy of the jar.
 * `verify-files` has already downloaded the jar to its usual place; a real
 * step could patch it or fetch it from somewhere else.
 */
static bool copied_jar(void *user_data, CobletStepCtx *ctx, const char *info_json) {
    (void)info_json;
    const Launch *launch = user_data;
    char original[4096], dir[4096], copy[4096];
    snprintf(original, sizeof original, "%s/versions/%s/%s.jar", launch->data_root, launch->version,
             launch->version);
    snprintf(dir, sizeof dir, "%s/acme", launch->data_root);
    mkdir(dir, 0755);
    snprintf(dir, sizeof dir, "%s/acme/jars", launch->data_root);
    mkdir(dir, 0755);
    snprintf(copy, sizeof copy, "%s/%s.jar", dir, launch->version);
    if (!copy_file(original, copy)) {
        coblet_step_ctx_fail(ctx, "cannot copy the client jar");
        return false;
    }
    return coblet_step_ctx_set_client_jar(ctx, copy, NULL) == COBLET_STATUS_OK;
}

/* Picks a session ID for each launch and hands it to the arguments. */
static bool session(void *user_data, CobletStepCtx *ctx, const char *info_json) {
    (void)user_data;
    (void)info_json;
    char id[64];
    snprintf(id, sizeof id, "%lx%lx", (unsigned long)time(NULL), (unsigned long)getpid());
    return coblet_step_ctx_set_placeholder(ctx, "acme_session", id, NULL) == COBLET_STATUS_OK;
}

/* Answers `${acme_from_source}` from what the launch is about. */
static bool machine(void *user_data, CobletPlaceholderCtx *ctx, const char *request_json) {
    (void)user_data;
    if (strstr(request_json, "\"acme_from_source\"") == NULL) {
        return true;
    }
    char version[256], os[64], arch[64], answer[512];
    if (!json_text(request_json, "game_version", version, sizeof version) ||
        !json_text(request_json, "os", os, sizeof os) ||
        !json_text(request_json, "arch", arch, sizeof arch)) {
        coblet_placeholder_ctx_fail(ctx, "the request is missing fields");
        return false;
    }
    snprintf(answer, sizeof answer, "{\"acme_from_source\":\"%s-%s-%s\"}", version, os, arch);
    return coblet_placeholder_ctx_set_result(ctx, answer) == COBLET_STATUS_OK;
}

/* Uses the placeholders; nothing here knows their values. */
static bool arguments(void *user_data, CobletProfileEdit *edit) {
    (void)user_data;
    const char *args[] = {"-Dacme.brand=${acme_brand}", "-Dacme.session=${acme_session}",
                          "-Dacme.source=${acme_from_source}"};
    for (size_t i = 0; i < sizeof args / sizeof args[0]; i++) {
        if (coblet_edit_add_arg(edit, COBLET_ARG_KIND_JVM, args[i], false, NULL) !=
            COBLET_STATUS_OK) {
            coblet_edit_fail(edit, "cannot add the jvm argument");
            return false;
        }
    }
    return true;
}

static int fail(const char *what, CobletError *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err->message.ptr : "(no details)");
    coblet_error_free(err);
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s <data root> <version> <marker>\n", argv[0]);
        return 2;
    }
    Launch launch = {argv[1], argv[2]};
    CobletError *err = NULL;
    CobletBuilder *builder = NULL;
    if (coblet_builder_new(argv[1], "coblet-ffi-launch-extensions/0.1", &builder, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_builder_new", err);
    }
    CobletStep jar = {"acme-copied-jar", &launch, copied_jar, NULL};
    if (coblet_builder_replace_step(builder, "client-jar", jar, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_replace_step", err);
    }
    if (coblet_builder_remove_step(builder, "logging-config", &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_remove_step", err);
    }
    CobletStep step = {"acme-session", NULL, session, NULL};
    if (coblet_builder_add_step(builder, "pre-launch", false, step, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_step", err);
    }
    if (coblet_builder_add_placeholder(builder, "acme_brand", "acme-launcher", &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_builder_add_placeholder", err);
    }
    CobletPlaceholderSource source = {NULL, "acme:machine", "Acme machine", machine, NULL};
    if (coblet_builder_add_placeholder_source(builder, source, 0, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_placeholder_source", err);
    }
    CobletCustomizer customizer = {"acme:arguments", "Acme arguments", NULL, NULL, arguments, NULL};
    if (coblet_builder_add_customizer(builder, customizer, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_customizer", err);
    }
    Coblet *coblet = NULL;
    if (coblet_builder_build(builder, &coblet, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_build", err);
    }

    CobletString json = {0};
    if (coblet_launch_steps_json(coblet, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_launch_steps_json", err);
    }
    printf("steps: %s\n", json.ptr);
    coblet_string_free(json);
    if (coblet_placeholder_sources_json(coblet, &json, &err) != COBLET_STATUS_OK) {
        return fail("coblet_placeholder_sources_json", err);
    }
    printf("placeholder sources: %s\n", json.ptr);
    coblet_string_free(json);

    CobletGame *game = NULL;
    const char *chosen[] = {"acme:arguments"};
    if (coblet_launch_customized(coblet, argv[2], "Steve", COBLET_LAUNCH_MODE_OFFLINE, NULL, chosen,
                                 1, NULL, &game, &err) != COBLET_STATUS_OK) {
        return fail("coblet_launch_customized", err);
    }
    CobletString report = {0};
    if (coblet_game_report(game, &report, &err) != COBLET_STATUS_OK) {
        return fail("coblet_game_report", err);
    }
    for (char *line = strtok(report.ptr, "\n"); line != NULL; line = strtok(NULL, "\n")) {
        if (strncmp(line, "placeholder ", strlen("placeholder ")) == 0) {
            printf("report: %s\n", line);
        }
    }
    coblet_string_free(report);

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
    printf("exit: killed %d, %llu ms\n", exit_info.killed,
           (unsigned long long)exit_info.duration_ms);
    coblet_game_free(game);
    coblet_free(coblet);
    return reached ? 0 : 1;
}
