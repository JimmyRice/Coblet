"""Smoke test of the coblet UniFFI bindings: list versions, launch one, stop
it once a log message contains a marker.

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


async def main(data_root: str, version: str, mode: str, marker: str) -> int:
    sdk = coblet.CobletSdk(data_root, "coblet-uniffi-smoke/0.1")
    versions = await sdk.versions()
    latest = await sdk.latest_release()
    print(f"{len(versions)} versions, latest release {latest.id}")

    launch_mode = coblet.LaunchMode.DEMO if mode == "demo" else coblet.LaunchMode.OFFLINE
    game = await sdk.launch(version, "Steve", launch_mode, None)
    print(f"started pid {game.pid()}")
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
