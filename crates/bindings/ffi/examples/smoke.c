/*
 * Smoke test of the coblet C ABI: list versions, launch one with a TOML
 * Platform Profile, a C Platform Profile and a customizer that each add a
 * JVM property, plus an extra step, print their lines of the launch
 * report and the launch events (steps, download totals, game start), stop
 * the game once a log message contains a marker.
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

/* Decides in code; a real one would probe the machine for libraries. */
static bool profile_matches(void *user_data, const char *target_json, CobletMatchFailure *failure) {
    (void)user_data;
    (void)failure;
    return strstr(target_json, "\"uid\":\"net.minecraft\"") != NULL;
}

static bool profile_rewrite(void *user_data, CobletProfileDraft *draft) {
    (void)user_data;
    if (coblet_draft_append_arg(draft, COBLET_ARG_KIND_JVM, "-Dcoblet.smoke.code-profile=applied",
                                NULL) != COBLET_STATUS_OK) {
        coblet_draft_fail(draft, "cannot add the jvm argument");
        return false;
    }
    return coblet_draft_warn(draft, "smoke profile written in C", NULL) == COBLET_STATUS_OK;
}

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

/* Prints the events of one launch that arrived so far, without waiting. */
static void print_launch_events(CobletEvents *events, const char *launch_id) {
    CobletEvent event = {0};
    CobletError *err = NULL;
    unsigned transfers = 0;
    while (coblet_events_next(events, 0, &event, &err) == COBLET_STATUS_OK) {
        if (event.kind == COBLET_EVENT_KIND_MISSED) {
            /* Read after the launch, a cold download can outgrow the buffer. */
            printf("event: missed %llu\n", (unsigned long long)event.missed);
        } else if (strcmp(event.op, launch_id) == 0) {
            if (event.kind == COBLET_EVENT_KIND_TRANSFER) {
                transfers++;
                if (event.files_done == event.files_total) {
                    printf("event: transfer %d done, %llu files, %llu bytes\n", event.transfer,
                           (unsigned long long)event.files_total,
                           (unsigned long long)event.bytes_done);
                }
            } else if (event.kind != COBLET_EVENT_KIND_GAME_LOG && event.json.ptr != NULL) {
                printf("event %d: %s\n", event.kind, event.json.ptr);
            }
        }
        coblet_event_clear(&event);
    }
    coblet_error_free(err);
    printf("event: %u transfer updates\n", transfers);
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
    if (coblet_builder_add_platform_profile_toml(builder, SMOKE_PROFILE, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_platform_profile_toml", err);
    }
    CobletPlatformProfile code_profile = {"coblet-smoke:code-profile", "Smoke code profile", 100, NULL,
                                          profile_matches, NULL, profile_rewrite, NULL};
    if (coblet_builder_add_platform_profile(builder, code_profile, &err) != COBLET_STATUS_OK) {
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

    CobletString platform = {0};
    if (coblet_platform_json(coblet, &platform, &err) != COBLET_STATUS_OK) {
        return fail("coblet_platform_json", err);
    }
    printf("platform: %s\n", platform.ptr);
    coblet_string_free(platform);

    CobletString versions = {0};
    if (coblet_versions_json(coblet, &versions, &err) != COBLET_STATUS_OK) {
        return fail("coblet_versions_json", err);
    }
    printf("versions json: %zu bytes, starts with %.120s\n", versions.len, versions.ptr);
    coblet_string_free(versions);

    CobletLaunchMode mode = strcmp(argv[3], "demo") == 0 ? COBLET_LAUNCH_MODE_DEMO
                                                         : COBLET_LAUNCH_MODE_OFFLINE;
    /* Subscribing before the launch keeps its events until they are read. */
    CobletEvents *events = NULL;
    char launch_id[COBLET_OPERATION_ID_LEN];
    if (coblet_events_subscribe(coblet, &events, &err) != COBLET_STATUS_OK ||
        coblet_generate_operation_id(launch_id, &err) != COBLET_STATUS_OK) {
        return fail("coblet_events_subscribe", err);
    }
    CobletGame *game = NULL;
    const char *chosen[] = {"coblet-smoke:marker"};
    CobletStatus launched = coblet_launch_customized(coblet, argv[2], "Steve", mode, NULL, chosen,
                                                     1, launch_id, &game, &err);
    if (launched != COBLET_STATUS_OK) {
        return fail("coblet_launch_customized", err);
    }
    print_launch_events(events, launch_id);
    coblet_events_free(events);
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
