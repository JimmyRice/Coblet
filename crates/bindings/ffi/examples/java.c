/*
 * Java example of the coblet C ABI, online: Mojang's runtimes are removed
 * and the machine's own Java is hidden, so the launch has to install Java
 * from the next provider that has it (Adoptium, then Azul Zulu). A
 * thread of its own reads the launch's events while the launch call
 * blocks, printing the steps and the Java download every 10%. A locator written in C adds the `java` named by the
 * COBLET_EXAMPLE_JAVA environment variable, and a provider described in
 * TOML shows the data form. The game is stopped once a log message
 * contains the marker, and the installed runtimes are listed.
 *
 *   cc -pthread -I include examples/java.c -L <target dir> -lcoblet_ffi -o java
 *   ./java <data root> <version> <marker>
 */
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "coblet.h"

/* A provider described as data: one pinned archive, offered only on
 * FreeBSD, so it never wins here but shows the format. */
static const char *PINNED_PROVIDER =
    "id = \"example:pinned\"\n"
    "name = \"Pinned FreeBSD JRE\"\n"
    "[packages]\n"
    "format = \"static\"\n"
    "[[packages.list]]\n"
    "id = \"pinned-17-freebsd-x86_64\"\n"
    "version = \"17.0.12\"\n"
    "major = 17\n"
    "os = \"freebsd\"\n"
    "arch = \"x86_64\"\n"
    "url = \"https://java.example.test/17.tar.gz\"\n"
    "hash = \"sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\"\n";

/* Hidden so the launch has to install a runtime; coblet:runtimes stays. */
static const char *HIDDEN_LOCATORS[] = {"coblet:java-home", "coblet:path", "coblet:sdkman",
                                        "coblet:system"};

/* Lists the executable in COBLET_EXAMPLE_JAVA, if any, as a JSON array. */
static bool locate(void *user_data, CobletJavaCtx *ctx, const char *os) {
    (void)user_data;
    (void)os;
    const char *java = getenv("COBLET_EXAMPLE_JAVA");
    char json[1024] = "[]";
    if (java != NULL && strchr(java, '"') == NULL && strchr(java, '\\') == NULL) {
        snprintf(json, sizeof json, "[\"%s\"]", java);
    }
    return coblet_java_ctx_set_result(ctx, json) == COBLET_STATUS_OK;
}

static int fail(const char *what, CobletError *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err->message.ptr : "(no details)");
    coblet_error_free(err);
    return 1;
}

typedef CobletStatus (*JsonCall)(const Coblet *, CobletString *, CobletError **);

/* Prints the JSON document one of the coblet_java_*_json calls returns. */
static int print_json(const char *what, const Coblet *coblet, JsonCall call) {
    CobletString json = {0};
    CobletError *err = NULL;
    if (call(coblet, &json, &err) != COBLET_STATUS_OK) {
        return fail(what, err);
    }
    printf("%s: %s\n", what, json.ptr);
    coblet_string_free(json);
    return 0;
}

/* Reads the events of one launch on its own thread while the launch blocks. */
typedef struct {
    CobletEvents *events;
    const char *launch_id;
    atomic_bool launch_returned;
} LaunchReader;

static void print_java_progress(const CobletEvent *event, unsigned *shown) {
    unsigned percent = event->has_bytes_total && event->bytes_total > 0
                           ? (unsigned)(event->bytes_done * 100 / event->bytes_total)
                           : 0;
    bool done = event->files_done == event->files_total;
    if (done || percent / 10 != *shown / 10) {
        *shown = percent;
        printf("event: java runtime %u%%, %llu/%llu files, %llu bytes\n", percent,
               (unsigned long long)event->files_done, (unsigned long long)event->files_total,
               (unsigned long long)event->bytes_done);
    }
}

static void *read_launch_events(void *arg) {
    LaunchReader *reader = arg;
    CobletEvent event = {0};
    CobletError *err = NULL;
    unsigned shown = 0;
    bool finished = false;
    while (!finished) {
        CobletStatus status = coblet_events_next(reader->events, 100, &event, &err);
        if (status == COBLET_STATUS_TIMEOUT) {
            /* Events published before the launch returned are already queued. */
            if (atomic_load(&reader->launch_returned)) {
                break;
            }
            continue;
        }
        if (status != COBLET_STATUS_OK) {
            break;
        }
        if (event.kind == COBLET_EVENT_KIND_MISSED) {
            printf("event: missed %llu\n", (unsigned long long)event.missed);
        } else if (strcmp(event.op, reader->launch_id) == 0) {
            if (event.kind == COBLET_EVENT_KIND_TRANSFER &&
                event.transfer == COBLET_TRANSFER_KIND_JAVA_RUNTIME) {
                print_java_progress(&event, &shown);
            } else if (event.kind == COBLET_EVENT_KIND_LAUNCH_STEP && event.json.ptr != NULL) {
                printf("event: step %s\n", event.json.ptr);
            } else if (event.kind == COBLET_EVENT_KIND_OPERATION_FINISHED) {
                printf("event: launch finished %s\n", event.json.ptr ? event.json.ptr : "");
                finished = true;
            }
        }
        coblet_event_clear(&event);
    }
    coblet_error_free(err);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s <data root> <version> <marker>\n", argv[0]);
        return 2;
    }
    printf("abi version %u\n", coblet_abi_version());

    CobletError *err = NULL;
    CobletBuilder *builder = NULL;
    if (coblet_builder_new(argv[1], "CobletJavaExample/1.0 (example@example.com)", &builder,
                           &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_new", err);
    }
    if (coblet_builder_remove_java_provider(builder, "coblet:mojang", &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_remove_java_provider", err);
    }
    if (coblet_builder_add_java_provider_toml(builder, PINNED_PROVIDER, 0, &err) !=
        COBLET_STATUS_OK) {
        return fail("coblet_builder_add_java_provider_toml", err);
    }
    for (size_t i = 0; i < sizeof HIDDEN_LOCATORS / sizeof HIDDEN_LOCATORS[0]; i++) {
        if (coblet_builder_remove_java_locator(builder, HIDDEN_LOCATORS[i], &err) !=
            COBLET_STATUS_OK) {
            return fail("coblet_builder_remove_java_locator", err);
        }
    }
    CobletJavaLocator locator = {NULL, "example:env", "COBLET_EXAMPLE_JAVA", locate, NULL};
    if (coblet_builder_add_java_locator(builder, locator, 0, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_add_java_locator", err);
    }
    Coblet *coblet = NULL;
    if (coblet_builder_build(builder, &coblet, &err) != COBLET_STATUS_OK) {
        return fail("coblet_builder_build", err);
    }

    if (print_json("providers", coblet, coblet_java_providers_json) ||
        print_json("locators", coblet, coblet_java_locators_json) ||
        print_json("installed before", coblet, coblet_java_installs_json)) {
        return 1;
    }

    /* Subscribing before the launch keeps its events until they are read. */
    CobletEvents *events = NULL;
    char launch_id[COBLET_OPERATION_ID_LEN];
    if (coblet_events_subscribe(coblet, &events, &err) != COBLET_STATUS_OK ||
        coblet_generate_operation_id(launch_id, &err) != COBLET_STATUS_OK) {
        return fail("coblet_events_subscribe", err);
    }
    LaunchReader reader = {events, launch_id, false};
    pthread_t reader_thread;
    if (pthread_create(&reader_thread, NULL, read_launch_events, &reader) != 0) {
        fprintf(stderr, "cannot start the event reader\n");
        return 1;
    }
    CobletGame *game = NULL;
    CobletStatus launched = coblet_launch_customized(
        coblet, argv[2], "Steve", COBLET_LAUNCH_MODE_OFFLINE, NULL, NULL, 0, launch_id, &game, &err);
    atomic_store(&reader.launch_returned, true);
    pthread_join(reader_thread, NULL);
    coblet_events_free(events);
    if (launched != COBLET_STATUS_OK) {
        return fail("coblet_launch_customized", err);
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
    printf("exit: killed %d, %llu ms\n", exit_info.killed,
           (unsigned long long)exit_info.duration_ms);
    coblet_game_free(game);

    if (print_json("installed after", coblet, coblet_java_installs_json)) {
        return 1;
    }
    coblet_free(coblet);
    return reached ? 0 : 1;
}
