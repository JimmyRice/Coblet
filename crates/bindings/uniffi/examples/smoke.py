"""Smoke test of the coblet UniFFI bindings: list versions, launch one with a
TOML Platform Profile, a Python Platform Profile and a customizer that each
add a JVM property, plus an extra step, print their lines of the launch
report and the launch events (steps, download totals, game start), stop the
game once a log message contains a marker.

    cargo build -p coblet-uniffi
    cargo run -p coblet-uniffi --features cli --bin uniffi-bindgen -- \
        generate --library target/debug/libcoblet_uniffi.dylib \
        --language python --out-dir target/uniffi-py
    cp target/debug/libcoblet_uniffi.dylib target/uniffi-py/
    PYTHONPATH=target/uniffi-py python3 examples/smoke.py <data root> <version> <offline|demo> <marker>
"""

import asyncio
import sys

import coblet_uniffi as coblet


# Matches every platform; a real profile would adapt a new OS or architecture.
SMOKE_PROFILE = """
id = "coblet-smoke:profile"
[jvm]
append = ["-Dcoblet.smoke.profile=applied"]
"""


class CodeProfile(coblet.PlatformProfile):
    """Decides in code; a real one would probe the machine for libraries."""

    def __init__(self) -> None:
        self.version = ""

    def id(self) -> str:
        return "coblet-smoke:code-profile"

    def display_name(self) -> str:
        return "Smoke code profile"

    def priority(self) -> int:
        return 100

    def matches(self, target: coblet.ProfileTarget) -> bool:
        self.version = target.game_version
        return any(c.uid == "net.minecraft" for c in target.components)

    def rules(self, target: coblet.ProfileTarget) -> coblet.RuleOverrides:
        return coblet.RuleOverrides()

    def rewrite(self, draft: coblet.ProfileDraft) -> None:
        natives = sum(1 for library in draft.libraries() if library.has_natives)
        draft.append_jvm_arg(f"-Dcoblet.smoke.code-profile={self.version}")
        draft.warn(f"smoke profile saw {natives} libraries with natives")


class Step(coblet.LaunchStep):
    def __init__(self) -> None:
        self.ran = False

    def name(self) -> str:
        return "coblet-smoke-step"

    def run(self, info: coblet.StepInfo) -> None:
        print(f"extra step ran: {info}")
        self.ran = True


class Marker(coblet.LaunchCustomizer):
    def __init__(self) -> None:
        self.step = Step()

    def id(self) -> str:
        return "coblet-smoke:marker"

    def display_name(self) -> str:
        return "Smoke marker"

    def applies_to(self, traits: list[str]) -> bool:
        return True

    def customize(self, editor: coblet.ProfileEditor) -> None:
        editor.add_jvm_arg("-Dcoblet.smoke=customized", False)
        editor.add_step_after("pre-launch", self.step)


async def print_launch_events(stream, launch_id: str) -> None:
    """Prints the steps, finished transfers and start of one launch."""
    transfers = 0
    while (event := await stream.next()) is not None:
        if isinstance(event, coblet.CobletEvent.MISSED):
            print(f"event: missed {event.count}")
            continue
        if getattr(event, "op", None) != launch_id:
            continue
        if isinstance(event, coblet.CobletEvent.LAUNCH_STEP):
            print(f"event: step {event.index + 1}/{event.count} {event.step}")
        elif isinstance(event, coblet.CobletEvent.TRANSFER):
            transfers += 1
            if event.files_done == event.files_total:
                print(f"event: transfer {event.kind} done, {event.files_total} files, "
                      f"{event.bytes_done} bytes")
        elif isinstance(event, coblet.CobletEvent.GAME_STARTED):
            print(f"event: game started, pid {event.pid}, {transfers} transfer updates")
        elif isinstance(event, coblet.CobletEvent.OPERATION_FINISHED):
            print(f"event: launch finished, {event.outcome}")
            return


async def main(data_root: str, version: str, mode: str, marker: str) -> int:
    marker_customizer = Marker()
    builder = coblet.CobletBuilder(data_root, "coblet-uniffi-smoke/0.1")
    builder.add_platform_profile_toml(SMOKE_PROFILE)
    builder.add_platform_profile(CodeProfile())
    builder.add_customizer(marker_customizer)
    launcher = builder.build()
    print(f"platform: {launcher.platform()}")
    versions = await launcher.versions()
    latest = await launcher.latest_release()
    print(f"{len(versions)} versions, latest release {latest.id}")

    launch_mode = coblet.LaunchMode.DEMO if mode == "demo" else coblet.LaunchMode.OFFLINE
    launch_id = coblet.generate_operation_id()
    options = coblet.LaunchOptions(
        player_name="Steve",
        mode=launch_mode,
        customizers=["coblet-smoke:marker"],
        operation_id=launch_id,
    )
    # Subscribing before the launch keeps its events until they are read.
    events = asyncio.create_task(print_launch_events(launcher.subscribe(), launch_id))
    game = await launcher.launch_with(version, options)
    await events
    print(f"started pid {game.pid()}")
    for report_line in game.report().splitlines():
        if "coblet-smoke:" in report_line:
            print(f"report: {report_line}")
    print(f"extra step ran before start: {marker_customizer.step.ran}")
    reached = False
    while (line := await game.next_log()) is not None:
        print(f"[{line.source} {line.level}] {line.message}")
        if not reached and marker in line.message:
            print("reached marker, killing the game")
            reached = True
            game.kill()
    exit_info = await game.wait()
    print(f"exit: {exit_info}")
    return 0 if reached else 1


if __name__ == "__main__":
    if len(sys.argv) != 5:
        print(__doc__)
        sys.exit(2)
    sys.exit(asyncio.run(main(*sys.argv[1:])))
