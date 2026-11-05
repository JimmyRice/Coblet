"""Smoke test of the coblet UniFFI bindings: list versions, launch one with a
Platform Profile and a customizer that each add a JVM property, plus an
extra step, print their lines of the launch report, stop the game once a log
message contains a marker.

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


async def main(data_root: str, version: str, mode: str, marker: str) -> int:
    marker_customizer = Marker()
    builder = coblet.CobletBuilder(data_root, "coblet-uniffi-smoke/0.1")
    builder.add_platform_profile_toml(SMOKE_PROFILE)
    builder.add_customizer(marker_customizer)
    launcher = builder.build()
    print(f"platform: {launcher.platform()}")
    versions = await launcher.versions()
    latest = await launcher.latest_release()
    print(f"{len(versions)} versions, latest release {latest.id}")

    launch_mode = coblet.LaunchMode.DEMO if mode == "demo" else coblet.LaunchMode.OFFLINE
    options = coblet.LaunchOptions(
        player_name="Steve", mode=launch_mode, customizers=["coblet-smoke:marker"]
    )
    game = await launcher.launch_with(version, options)
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
