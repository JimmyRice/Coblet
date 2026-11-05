/*
 * Smoke test of the coblet C ABI: list versions, launch one with a
 * Platform Profile and a customizer that each add a JVM property, plus an
 * extra step, print their lines of the launch report, stop the game once
 * a log message contains a marker.
 *
 *   cc -I include examples/smoke.c -L <target dir> -lcoblet_ffi -o smoke
 *   ./smoke <data root> <version> <offline|demo> <marker>
 */
#include <stdio.h>
#include <string.h>

#include "coblet.h"

/* Matches every platform; a real profile would adapt a new OS or architecture. */
static const char *SMOKE_PROFILE = "id = \"coblet-smoke:profile\"\n"
                                   "[jvm]\n"
                                   "append = [\"-Dcoblet.smoke.profile=applied\"]\n";

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

    CobletError *err = NULL;
    CobletBuilder *builder = NULL;
    if (coblet_builder_new(argv[1], "coblet-ffi-smoke/0.1", &builder, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_new", err);
    }
    if (coblet_builder_add_platform_profile(builder, SMOKE_PROFILE, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_platform_profile", err);
    }
    CobletCustomizer customizer = {"coblet-smoke:marker", "Smoke marker", NULL, NULL, customize, NULL};
    if (coblet_builder_add_customizer(builder, customizer, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_customizer", err);
    }
    Coblet *coblet = NULL;
    if (coblet_builder_build(builder, &coblet, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_build", err);
    }

    CobletString versions = {0};
    if (coblet_versions_json(coblet, &versions, &err) != COBLET_STATUS_OK) {
        return fail("coblet_versions_json", err);
    }
    printf("versions json: %zu bytes, starts with %.120s\n", versions.len, versions.ptr);
    coblet_string_free(versions);

    CobletLaunchMode mode = strcmp(argv[3], "demo") == 0 ? COBLET_LAUNCH_MODE_DEMO
                                                         : COBLET_LAUNCH_MODE_OFFLINE;
    CobletGame *game = NULL;
    const char *chosen[] = {"coblet-smoke:marker"};
    if (coblet_launch_customized(coblet, argv[2], "Steve", mode, NULL, chosen, 1, &game, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_launch_customized", err);
    }
    CobletString report = {0};
    if (coblet_game_report(game, &report, &err) != COBLET_STATUS_OK) {
        return fail("coblet_game_report", err);
    }
    for (char *line = strtok(report.ptr, "\n"); line != NULL; line = strtok(NULL, "\n")) {
        if (strstr(line, "coblet-smoke:") != NULL) {
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
    coblet_free(coblet);
    return reached ? 0 : 1;
}
