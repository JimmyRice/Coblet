/*
 * Smoke test of the coblet C ABI: list versions, launch one with a
 * customizer that adds a JVM property and an extra step, print the
 * customizer lines of the launch report, stop the game once a log message
 * contains a marker.
 *
 *   cc -I include examples/smoke.c -L <target dir> -lcoblet_ffi -o smoke
 *   ./smoke <data root> <version> <offline|demo> <marker>
 */
#include <stdio.h>
#include <string.h>

#include "coblet.h"

static bool step_ran = false;

static bool run_step(void *user_data, const char *info_json, CobletStepFailure *failure) {
    (void)user_data;
    (void)failure;
    printf("extra step ran: %s\n", info_json);
    step_ran = true;
    return true;
}

static bool customize(void *user_data, CobletProfileEdit *edit) {
    (void)user_data;
    if (coblet_edit_add_arg(edit, COBLET_ARG_KIND_JVM, "-Dcoblet.smoke=customized", false, NULL) !=
        COBLET_STATUS_OK) {
        coblet_edit_fail(edit, "cannot add the jvm argument");
        return false;
    }
    CobletStep step = {"coblet-smoke-step", NULL, run_step, NULL};
    return coblet_edit_add_step(edit, "pre-launch", true, step, NULL) == COBLET_STATUS_OK;
}

static int fail(const char *what, CobletError *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err->message.ptr : "(no details)");
    coblet_error_free(err);
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 5) {
        fprintf(stderr, "usage: %s <data root> <version> <offline|demo> <marker>\n", argv[0]);
        return 2;
    }
    printf("abi version %u\n", coblet_abi_version());

    CobletSdk *sdk = NULL;
    CobletError *err = NULL;
    CobletCustomizer customizer = {"coblet-smoke:marker", "Smoke marker", NULL, NULL, customize, NULL};
    if (coblet_sdk_new_with_customizers(argv[1], "coblet-ffi-smoke/0.1", &customizer, 1, &sdk,
                                        &err) != COBLET_STATUS_OK) {
        return fail("coblet_sdk_new_with_customizers", err);
    }

    CobletString versions = {0};
    if (coblet_versions_json(sdk, &versions, &err) != COBLET_STATUS_OK) {
        return fail("coblet_versions_json", err);
    }
    printf("versions json: %zu bytes, starts with %.120s\n", versions.len, versions.ptr);
    coblet_string_free(versions);

    CobletLaunchMode mode = strcmp(argv[3], "demo") == 0 ? COBLET_LAUNCH_MODE_DEMO
                                                         : COBLET_LAUNCH_MODE_OFFLINE;
    CobletGame *game = NULL;
    const char *chosen[] = {"coblet-smoke:marker"};
    if (coblet_launch_customized(sdk, argv[2], "Steve", mode, NULL, chosen, 1, &game, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_launch_customized", err);
    }
    CobletString report = {0};
    if (coblet_game_report(game, &report, &err) != COBLET_STATUS_OK) {
        return fail("coblet_game_report", err);
    }
    for (char *line = strtok(report.ptr, "\n"); line != NULL; line = strtok(NULL, "\n")) {
        if (strncmp(line, "[customize]", 11) == 0) {
            printf("report: %s\n", line);
        }
    }
    coblet_string_free(report);
    printf("extra step ran before start: %d\n", step_ran);

    int reached = 0;
    CobletString line = {0};
    CobletStatus status;
    while ((status = coblet_game_next_log(game, &line, &err)) == COBLET_STATUS_OK) {
        printf("%s\n", line.ptr);
        if (!reached && strstr(line.ptr, argv[4]) != NULL) {
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
    printf("exit: code %d (set %d), killed %d, suspected crash %d, %llu ms\n", exit_info.code,
           exit_info.has_code, exit_info.killed, exit_info.suspected_crash,
           (unsigned long long)exit_info.duration_ms);
    coblet_game_free(game);
    coblet_sdk_free(sdk);
    return reached ? 0 : 1;
}
