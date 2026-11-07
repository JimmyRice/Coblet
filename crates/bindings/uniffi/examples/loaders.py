"""Mod Loader example of the coblet UniFFI bindings, offline: a loader
written in Python and one described in TOML list their versions, give an
install recipe and are resolved on top of a game version.

A local HTTP server stands in for the loader's metadata server and for
Mojang's, reached through mirror rules with fallback off, so nothing leaves
the machine; the Python loader's requests go through the same rules as the
built-in ones.

    cargo build -p coblet-uniffi
    cargo run -p coblet-uniffi --features cli --bin uniffi-bindgen -- \
        generate --library target/debug/libcoblet_uniffi.dylib \
        --language python --out-dir target/uniffi-py
    cp target/debug/libcoblet_uniffi.dylib target/uniffi-py/
    PYTHONPATH=target/uniffi-py python3 crates/bindings/uniffi/examples/loaders.py <data root>
"""

import asyncio
import http.server
import json
import pathlib
import sys
import threading

import coblet_uniffi as coblet

FIXTURES = pathlib.Path(__file__).resolve().parents[4] / "fixtures" / "mojang"
LISTING = [{"version": "2.0", "stable": False}, {"version": "1.4.2", "stable": True}]

DATA_LOADER = """
id = "example:data-loader"
name = "Example Data Loader"
uid = "example.data.loader"
[versions]
format = "json"
url = "https://meta.example.test/versions/{game}"
id = "/version"
stable = "/stable"
[profile]
format = "static"
[[profile.components]]
uid = "example.data.loader"
version = "{version}"
requires = [{ uid = "net.minecraft", equals = "{game}" }]
mainClass = "example.data.Main"
libraries = [{ name = "example.data:loader:{version}", url = "https://maven.example.test/" }]
"""


class Handler(http.server.BaseHTTPRequestHandler):
    """Serves the Mojang fixtures by file name and the loader listing."""

    def do_GET(self) -> None:
        if self.path.startswith("/meta/versions/"):
            body = json.dumps(LISTING).encode()
        else:
            file = FIXTURES / self.path.rsplit("/", 1)[-1]
            if not file.is_file():
                self.send_error(404)
                return
            body = file.read_bytes()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        print("  served", self.path)


class PythonLoader(coblet.ModLoader):
    """Lists versions over the context and returns the recipe as JSON."""

    def id(self) -> str:
        return "example:python-loader"

    def display_name(self) -> str:
        return "Example Python Loader"

    def component_uid(self) -> str:
        return "example.python.loader"

    def versions(self, cx: coblet.LoaderContext, game: str) -> list[coblet.LoaderVersionInfo]:
        listed = json.loads(
            cx.fetch_text(f"https://meta.example.test/versions/{game}", f"versions/{game}.json")
        )
        stable = [entry["version"] for entry in listed if entry["stable"]]
        return [
            coblet.LoaderVersionInfo(
                id=entry["version"],
                game=game,
                version_type="release" if entry["stable"] else "beta",
                recommended=bool(stable) and entry["version"] == stable[0],
                data_json=json.dumps({"tweaker": "example.python.Tweaker"}),
            )
            for entry in listed
        ]

    def install(
        self, cx: coblet.LoaderContext, game: str, version: coblet.LoaderVersionInfo
    ) -> str:
        data = json.loads(version.data_json or "{}")
        return json.dumps({"components": [{
            "uid": "example.python.loader",
            "version": version.id,
            "requires": [{"uid": "net.minecraft", "equals": game}],
            "mainClass": "example.python.Main",
            "+tweakers": [data["tweaker"]],
            "libraries": [{
                "name": f"example.python:loader:{version.id}",
                "url": "https://maven.example.test/",
            }],
        }]})


async def main(data_root: str) -> None:
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    local = f"http://127.0.0.1:{server.server_port}"

    builder = coblet.CobletBuilder(data_root, "CobletLoaderExample/1.0 (example@example.com)")
    builder.set_platform(coblet.PlatformInfo(os="linux", arch="x86_64"))
    builder.add_mirror("https://piston-meta.mojang.com/", f"{local}/mojang/", [], False)
    builder.add_mirror(
        "https://meta.example.test/", f"{local}/meta/", [coblet.ArtifactKind.LOADER()], False
    )
    builder.set_mirror_fallback(False)
    builder.add_loader(PythonLoader())
    builder.add_loader_toml(DATA_LOADER)
    sdk = builder.build()

    for loader in sdk.loaders():
        print(f"{loader.id}: {loader.name} ({loader.component_uid})")
        versions = await sdk.loader_versions(loader.id, "1.20.1")
        print("  versions:", ", ".join(f"{v.id} [{v.version_type}]" for v in versions))
        chosen = await sdk.recommended_loader_version(loader.id, "1.20.1")
        assert chosen is not None and chosen.id == "1.4.2", chosen
        recipe = json.loads(await sdk.loader_recipe(loader.id, "1.20.1", chosen.id))
        print("  recipe:", [c["uid"] for c in recipe["components"]])
        plan = await sdk.plan_launch("1.20.1", coblet.LoaderChoice(loader=loader.id, version=chosen.id))
        print("  main class:", plan.main_class)
        print("  java:", plan.java_major)
        print("  loader libraries:", [n for n in plan.classpath if n.startswith("example.")])
        tweak = [plan.game_args[i + 1] for i, a in enumerate(plan.game_args[:-1]) if a == "--tweakClass"]
        print("  tweakers:", tweak)

    try:
        await sdk.loader_versions("example:missing", "1.20.1")
    except coblet.CobletError.Failed as error:
        print("unknown loader:", error.kind, error.message)
    server.shutdown()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    asyncio.run(main(sys.argv[1]))
