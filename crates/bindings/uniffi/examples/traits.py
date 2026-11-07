"""Version trait example of the coblet UniFFI bindings, online: a trait
table in TOML and a trait source written in Python tag versions, a TOML
Platform Profile and a customizer act on those traits, and a version is
launched until a log message contains the marker.

The table adds `acme:offline-skins` to b1.7.3 and to every version of a
second manifest, and removes the built-in `legacy-macos-rendering` from
b1.7.3. The Python source adds `acme:from-code` to old betas. A local HTTP
server serves the second manifest, so its versions are listed with their
source ID.

    cargo build -p coblet-uniffi
    cargo run -p coblet-uniffi --features cli --bin uniffi-bindgen -- \
        generate --library target/debug/libcoblet_uniffi.dylib \
        --language python --out-dir target/uniffi-py
    cp target/debug/libcoblet_uniffi.dylib target/uniffi-py/
    PYTHONPATH=target/uniffi-py python3 crates/bindings/uniffi/examples/traits.py <data root> <version> <marker>
"""

import asyncio
import http.server
import json
import sys
import threading

import coblet_uniffi as coblet

TABLE = """
id = "acme:fixes"
name = "Acme fixes"

[[rule]]
add = ["acme:offline-skins"]
ids = ["b1.7.3"]

[[rule]]
add = ["acme:offline-skins"]
sources = ["acme:experiments"]

[[rule]]
remove = ["legacy-macos-rendering"]
ids = ["b1.7.3"]
"""

PROFILE = """
id = "acme:offline-skins"

[match]
version_traits = ["acme:offline-skins"]

[jvm]
append = ["-Dacme.traits=ok"]
"""

EXPERIMENTS = {
    "latest": {"release": "acme-1", "snapshot": "acme-1"},
    "versions": [{
        "id": "acme-1",
        "type": "release",
        "url": "https://experiments.example.test/acme-1.json",
        "time": "2026-01-01T00:00:00+00:00",
        "releaseTime": "2026-01-01T00:00:00+00:00",
        "sha1": "0000000000000000000000000000000000000000",
        "complianceLevel": 1,
    }],
}


class Handler(http.server.BaseHTTPRequestHandler):
    """Serves the second manifest."""

    def do_GET(self) -> None:
        body = json.dumps(EXPERIMENTS).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        print("  served", self.path)


class OldBetas(coblet.TraitSource):
    """Adds `acme:from-code` to old betas and counts its batches."""

    def __init__(self) -> None:
        self.batches = []

    def id(self) -> str:
        return "acme:from-code"

    def display_name(self) -> str:
        return "Acme traits from code"

    def adjust(
        self, cx: coblet.TraitSourceContext, versions: list[coblet.TraitVersionInfo]
    ) -> list[list[str]]:
        self.batches.append((len(versions), versions[0].version_json is not None))
        return [
            v.traits + ["acme:from-code"] if v.version_type == "old_beta" else v.traits
            for v in versions
        ]


class Marker(coblet.LaunchCustomizer):
    """Adds a JVM property to versions the Python source tagged."""

    def id(self) -> str:
        return "acme:marker"

    def display_name(self) -> str:
        return "Acme marker"

    def applies_to(self, traits: list[str]) -> bool:
        return "acme:from-code" in traits

    def customize(self, editor: coblet.ProfileEditor) -> None:
        editor.add_jvm_arg("-Dacme.code=ok", False)


async def main(data_root: str, version: str, marker: str) -> int:
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    source = OldBetas()
    builder = coblet.CobletBuilder(data_root, "CobletTraitExample/1.0 (example@example.com)")
    builder.add_version_manifest(
        "acme:experiments", f"http://127.0.0.1:{server.server_port}/manifest.json"
    )
    builder.add_trait_table_toml(TABLE)
    builder.add_trait_source(source)
    builder.add_platform_profile_toml(PROFILE)
    builder.add_customizer(Marker())
    sdk = builder.build()

    print("trait sources:", ", ".join(s.id for s in sdk.trait_sources()))
    listed = {v.id: v for v in await sdk.versions()}
    print("listing batches (size, with version JSON):", source.batches)
    for id in (version, "acme-1", "1.20.1"):
        info = listed[id]
        print(f"listed {id} from {info.source}: {info.traits}")

    report = await sdk.version_traits(version)
    print(f"{version} at launch: {report.traits}")
    for change in report.changes:
        print(f"  {change.action} {change.value} by {change.source}")

    options = coblet.LaunchOptions(
        player_name="Steve", mode=coblet.LaunchMode.OFFLINE, customizers=["acme:marker"]
    )
    game = await sdk.launch_with(version, options)
    print(f"started pid {game.pid()}")
    for line in game.report().splitlines():
        if "acme" in line:
            print("report:", line)
    reached = False
    while (line := await game.next_log()) is not None:
        if not reached and marker in line.message:
            print(f"reached marker: {line.message}")
            reached = True
            game.kill()
    print("exit:", await game.wait())
    return 0 if reached else 1


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    sys.exit(asyncio.run(main(*sys.argv[1:])))
