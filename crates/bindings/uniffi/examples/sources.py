"""Version source example of the coblet UniFFI bindings, online: Mojang's
manifest is removed and a game version source written in Python lists
versions instead. The source reads Mojang's manifest through the SDK,
hosts the chosen version's JSON on a local HTTP server and lists it from
there with the hash it computed. Because the source fetched that version
JSON through its context, the SDK keeps it for the launch, which does not
download it again; libraries and assets still come from Mojang. The
version is launched until a log message contains the marker.

The local server also stands in for a Mod Loader's metadata, so the
example loader gets versions from itself, from a source described in TOML
and from one written in Python, and the merged list shows who listed each.

    cargo build -p coblet-uniffi
    cargo run -p coblet-uniffi --features cli --bin uniffi-bindgen -- \
        generate --library target/debug/libcoblet_uniffi.dylib \
        --language python --out-dir target/uniffi-py
    cp target/debug/libcoblet_uniffi.dylib target/uniffi-py/
    PYTHONPATH=target/uniffi-py python3 crates/bindings/uniffi/examples/sources.py <data root> <version> <marker>
"""

import asyncio
import hashlib
import http.server
import json
import sys
import threading

import coblet_uniffi as coblet

MANIFEST = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"

LOADER = """
id = "example:loader"
name = "Example Loader"
uid = "example.loader"
[versions]
format = "static"
list = [{ id = "1.0", recommended = true }]
[profile]
format = "static"
[[profile.components]]
uid = "example.loader"
version = "{version}"
"""


class Handler(http.server.BaseHTTPRequestHandler):
    """Serves the version JSON files and a loader listing."""

    files: dict[str, bytes] = {}

    def do_GET(self) -> None:
        body = self.files.get(self.path)
        if body is None:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        print("  served", self.path)


class LocalVersions(coblet.GameVersionSource):
    """Hosts the chosen versions on the local server and lists them there."""

    def __init__(self, base: str, chosen: list[str]) -> None:
        self.base = base
        self.chosen = chosen

    def id(self) -> str:
        return "example:local"

    def display_name(self) -> str:
        return "Local versions"

    def versions(self, cx: coblet.SourceContext) -> list[coblet.GameVersionEntry]:
        manifest = json.loads(cx.fetch_text(MANIFEST, "mojang-manifest.json"))
        entries = []
        for entry in manifest["versions"]:
            if entry["id"] not in self.chosen:
                continue
            body = cx.fetch(entry["url"], f"versions/{entry['id']}.json")
            sha1 = hashlib.sha1(body).hexdigest()
            assert sha1 == entry["sha1"], entry["id"]
            Handler.files[f"/versions/{entry['id']}.json"] = body
            entries.append(coblet.GameVersionEntry(
                id=entry["id"],
                version_type=entry["type"],
                url=f"{self.base}/versions/{entry['id']}.json",
                sha1=sha1,
                released=entry["releaseTime"],
                latest_release=not entries,
                traits=["example:local"],
            ))
        return entries


class Nightly(coblet.LoaderVersionSource):
    """Lists a newer beta of the example loader from the local server."""

    def __init__(self, base: str) -> None:
        self.base = base

    def id(self) -> str:
        return "example:nightly"

    def display_name(self) -> str:
        return "Example nightly builds"

    def versions(self, cx: coblet.SourceContext, game: str) -> list[coblet.LoaderVersionInfo]:
        listed = json.loads(cx.fetch_text(f"{self.base}/nightly/{game}", f"{game}.json"))
        return [coblet.LoaderVersionInfo(id=entry, version_type="beta") for entry in listed]


def mirror_listing(base: str) -> str:
    return f"""
id = "example:mirror"
name = "Example mirror"
[versions]
format = "json"
url = "{base}/mirror/{{game}}"
id = "/version"
"""


async def main(data_root: str, version: str, marker: str) -> int:
    Handler.files[f"/nightly/{version}"] = json.dumps(["2.0-nightly"]).encode()
    Handler.files[f"/mirror/{version}"] = json.dumps([{"version": "1.0"}, {"version": "0.9"}]).encode()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_port}"

    builder = coblet.CobletBuilder(data_root, "CobletSourceExample/1.0 (example@example.com)")
    builder.remove_version_source("coblet:mojang")
    builder.add_version_source(LocalVersions(base, [version]))
    builder.add_loader_toml(LOADER)
    builder.add_loader_version_source_toml("example:loader", mirror_listing(base))
    builder.add_loader_version_source("example:loader", Nightly(base))
    sdk = builder.build()

    sources = [s.id for s in sdk.version_sources()]
    print("version sources:", sources)
    assert sources == ["example:local"], sources
    versions = await sdk.versions()
    for info in versions:
        print(f"listed {info.id} ({info.version_type}) from {info.source}: {info.traits}")
    assert [(v.id, v.source) for v in versions] == [(version, "example:local")]
    latest = await sdk.latest_release()
    print("latest release:", latest.id)
    assert latest.id == version

    loader_sources = [s.id for s in sdk.loader_version_sources("example:loader")]
    print("loader sources:", loader_sources)
    assert loader_sources == ["example:mirror", "example:nightly"], loader_sources
    merged = [(e.id, e.source) for e in await sdk.loader_versions("example:loader", version)]
    for id, source in merged:
        print(f"  loader {id} from {source}")
    assert sorted(merged) == [
        ("0.9", "example:mirror"),
        ("1.0", "example:loader"),
        ("2.0-nightly", "example:nightly"),
    ], merged

    options = coblet.LaunchOptions(player_name="Steve", mode=coblet.LaunchMode.OFFLINE)
    game = await sdk.launch_with(version, options)
    print(f"started pid {game.pid()}")
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
