"""Launch extensions from Python: replace the `client-jar` step with one that
runs the game from a copy of the client jar, remove `logging-config`, fill
`${name}` placeholders from a static value, a step and a placeholder source,
and let a customizer use them in JVM arguments. Prints the step list, the
placeholder lines of the launch report (names and origins, never values)
and, where `ps` exists, the command line of the running game, then stops
the game once a log message contains a marker.

    cargo build -p coblet-uniffi
    cargo run -p coblet-uniffi --features cli --bin uniffi-bindgen -- \
        generate --library target/debug/libcoblet_uniffi.dylib \
        --language python --out-dir target/uniffi-py
    cp target/debug/libcoblet_uniffi.dylib target/uniffi-py/
    PYTHONPATH=target/uniffi-py python3 examples/launch_extensions.py <data root> <version> <marker>
"""

import asyncio
import os
import shutil
import subprocess
import sys
import uuid

import coblet_uniffi as coblet


class CopiedJar(coblet.LaunchStep):
    """Stands in for `client-jar`: runs the game from a copy of the jar.

    `verify-files` has already downloaded the jar to its usual place; a real
    step could patch it or fetch it from somewhere else.
    """

    def name(self) -> str:
        return "acme-copied-jar"

    def run(self, cx: coblet.StepContext) -> None:
        version = cx.game_version()
        original = os.path.join(cx.data_root(), "versions", version, f"{version}.jar")
        copy = os.path.join(cx.data_root(), "acme", "jars", f"{version}.jar")
        os.makedirs(os.path.dirname(copy), exist_ok=True)
        shutil.copyfile(original, copy)
        cx.set_client_jar(copy)


class Session(coblet.LaunchStep):
    """Picks a session ID for each launch and hands it to the arguments."""

    def name(self) -> str:
        return "acme-session"

    def run(self, cx: coblet.StepContext) -> None:
        cx.set_placeholder("acme_session", uuid.uuid4().hex)


class Machine(coblet.PlaceholderSource):
    """Answers `${acme_from_source}` from what the launch is about."""

    def id(self) -> str:
        return "acme:machine"

    def display_name(self) -> str:
        return "Acme machine"

    def resolve(self, names: list[str], info: coblet.PlaceholderInfo) -> dict[str, str]:
        known = {"acme_from_source": f"{info.game_version}-{info.os}-{info.arch}"}
        return {name: known[name] for name in names if name in known}


class Arguments(coblet.LaunchCustomizer):
    """Uses the placeholders; nothing here knows their values."""

    def id(self) -> str:
        return "acme:arguments"

    def display_name(self) -> str:
        return "Acme arguments"

    def applies_to(self, traits: list[str]) -> bool:
        return True

    def customize(self, editor: coblet.ProfileEditor) -> None:
        editor.add_jvm_arg("-Dacme.brand=${acme_brand}", False)
        editor.add_jvm_arg("-Dacme.session=${acme_session}", False)
        editor.add_jvm_arg("-Dacme.source=${acme_from_source}", False)


def print_command(pid: int | None) -> None:
    """Prints the JVM properties the running game got and where its jar is."""
    if pid is None or shutil.which("ps") is None:
        return
    args = subprocess.run(
        ["ps", "-ww", "-o", "args=", "-p", str(pid)], capture_output=True, text=True
    ).stdout.split()
    for arg in args:
        if arg.startswith("-Dacme.") or arg.startswith("-Dlog4j"):
            print(f"command: {arg}")
    jars = [part for arg in args for part in arg.split(":") if "acme/jars" in part]
    print(f"command: client jar from the copy: {bool(jars)}")


async def main(data_root: str, version: str, marker: str) -> int:
    builder = coblet.CobletBuilder(data_root, "coblet-uniffi-launch-extensions/0.1")
    builder.replace_step("client-jar", CopiedJar())
    builder.remove_step("logging-config")
    builder.add_step_before("pre-launch", Session())
    builder.add_placeholder("acme_brand", "acme-launcher")
    builder.add_placeholder_source(Machine())
    builder.add_customizer(Arguments())
    launcher = builder.build()
    print(f"steps: {', '.join(launcher.launch_steps())}")
    print(f"placeholder sources: {[source.id for source in launcher.placeholder_sources()]}")

    options = coblet.LaunchOptions(
        player_name="Steve",
        mode=coblet.LaunchMode.OFFLINE,
        customizers=["acme:arguments"],
        operation_id=coblet.generate_operation_id(),
    )
    game = await launcher.launch_with(version, options)
    print(f"started pid {game.pid()}")
    for report_line in game.report().splitlines():
        if report_line.startswith("placeholder "):
            print(f"report: {report_line}")
    reached = False
    while (line := await game.next_log()) is not None:
        print(f"[{line.source} {line.level}] {line.message}")
        if not reached and marker in line.message:
            print("reached marker")
            reached = True
            print_command(game.pid())
            game.kill()
    print(f"exit: {await game.wait()}")
    return 0 if reached else 1


if __name__ == "__main__":
    if len(sys.argv) != 4:
        print(__doc__)
        sys.exit(2)
    sys.exit(asyncio.run(main(*sys.argv[1:])))
