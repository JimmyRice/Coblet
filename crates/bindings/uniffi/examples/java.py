"""Java example of the coblet UniFFI bindings, online: Mojang's runtimes are
removed and the machine's own Java is hidden, so Java comes only from the
other providers. A Java provider written in Python offers Temurin JDKs, a
locator written in Python finds Java listed in COBLET_EXAMPLE_JAVA, Java 17
is installed from the first provider that has it, with its download
events, and the version is launched with it until a log message contains
the marker.

    cargo build -p coblet-uniffi
    cargo run -p coblet-uniffi --features cli --bin uniffi-bindgen -- \
        generate --library target/debug/libcoblet_uniffi.dylib \
        --language python --out-dir target/uniffi-py
    cp target/debug/libcoblet_uniffi.dylib target/uniffi-py/
    PYTHONPATH=target/uniffi-py python3 crates/bindings/uniffi/examples/java.py <data root> <version> <marker>
"""

import asyncio
import json
import os
import pathlib
import sys

import coblet_uniffi as coblet

# Adoptium's names for coblet's systems and architectures.
ADOPTIUM_OS = {"macos": "mac"}
ADOPTIUM_ARCH = {"x86_64": "x64", "x86": "x32"}

# Hidden so the example has to install a runtime; coblet:runtimes stays.
HIDDEN_LOCATORS = ["coblet:java-home", "coblet:path", "coblet:sdkman", "coblet:system"]


class TemurinJdk(coblet.JavaProvider):
    """Offers full Temurin JDKs; the built-in Adoptium provider offers JREs.

    It only describes packages: coblet downloads, checks and unpacks them.
    """

    def id(self) -> str:
        return "example:temurin-jdk"

    def display_name(self) -> str:
        return "Temurin JDK (Python)"

    def packages(
        self, cx: coblet.JavaProviderContext, query: coblet.JavaQueryInfo
    ) -> list[coblet.JavaPackageInfo]:
        os_name = ADOPTIUM_OS.get(query.os, query.os)
        arch = ADOPTIUM_ARCH.get(query.arch, query.arch)
        url = (
            f"https://api.adoptium.net/v3/assets/latest/{query.major}/hotspot"
            f"?os={os_name}&architecture={arch}&image_type=jdk&vendor=eclipse"
        )
        listed = json.loads(cx.fetch_text(url, f"{query.major}/{os_name}-{arch}.json"))
        packages = []
        for entry in listed:
            package = entry["binary"]["package"]
            name = package["name"]
            fmt = "zip" if name.endswith(".zip") else "tar.gz"
            packages.append(coblet.JavaPackageInfo(
                id=name.removesuffix(".zip").removesuffix(".tar.gz"),
                version=entry["version"]["openjdk_version"],
                major=entry["version"]["major"],
                arch=query.arch,
                url=package["link"],
                hash="sha256:" + package["checksum"],
                format=fmt,
                size=package["size"],
            ))
        return packages


class EnvLocator(coblet.JavaLocator):
    """Finds the `java` executables listed in COBLET_EXAMPLE_JAVA."""

    def id(self) -> str:
        return "example:env"

    def display_name(self) -> str:
        return "COBLET_EXAMPLE_JAVA"

    def locate(self, os_name: str) -> list[str]:
        listed = os.environ.get("COBLET_EXAMPLE_JAVA", "")
        return [path for path in listed.split(os.pathsep) if pathlib.Path(path).is_file()]


async def print_transfers(stream: coblet.EventStream, op: str) -> None:
    """Prints the Java download progress of one operation, every 10%."""
    shown = -1
    while (event := await stream.next()) is not None:
        if not isinstance(event, coblet.CobletEvent.TRANSFER) or event.op != op:
            continue
        total = event.bytes_total or 0
        percent = event.bytes_done * 100 // total if total else 0
        if percent // 10 != shown // 10 or event.files_done == event.files_total:
            shown = percent
            print(f"  {event.kind}: {event.bytes_done}/{total} bytes, "
                  f"{event.files_done}/{event.files_total} files")


def describe(install: coblet.JavaInstallInfo) -> str:
    source = install.source + (f" {install.source_id}" if install.source_id else "")
    return f"java {install.version} ({install.arch}, {install.vendor}) from {source}: {install.executable}"


async def main(data_root: str, version: str, marker: str) -> int:
    builder = coblet.CobletBuilder(data_root, "CobletJavaExample/1.0 (example@example.com)")
    builder.remove_java_provider("coblet:mojang")
    builder.add_java_provider(TemurinJdk())
    for locator in HIDDEN_LOCATORS:
        builder.remove_java_locator(locator)
    builder.add_java_locator(EnvLocator())
    sdk = builder.build()

    print("providers:", ", ".join(p.id for p in sdk.java_providers()))
    print("locators:", ", ".join(l.id for l in sdk.java_locators()))
    print("installed before:")
    for install in await sdk.java_installs():
        print(" ", describe(install))

    print("packages for java 17:")
    answers = await sdk.java_packages(17)
    for answer in answers:
        if answer.error:
            print(f"  {answer.provider}: failed: {answer.error}")
        for package in answer.packages:
            print(f"  {answer.provider}: {package.id} {package.version} "
                  f"{package.format}, {package.size} bytes")
    provider, package = next((a.provider, a.packages[0]) for a in answers if a.packages)

    print(f"installing {package.id} from {provider}")
    op = coblet.generate_operation_id()
    progress = asyncio.create_task(print_transfers(sdk.subscribe(), op))
    install = await sdk.install_java(provider, package, op)
    progress.cancel()
    print("installed:", describe(install))
    print("installed after:")
    for known in await sdk.java_installs():
        print(" ", describe(known))

    # The launch picks the runtime just installed instead of downloading one.
    options = coblet.LaunchOptions(player_name="Steve", mode=coblet.LaunchMode.OFFLINE)
    game = await sdk.launch_with(version, options)
    print(f"started pid {game.pid()}")
    for line in game.report().splitlines():
        if "java" in line.lower():
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
