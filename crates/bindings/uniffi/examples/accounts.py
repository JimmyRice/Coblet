"""Signs in to Microsoft through the coblet UniFFI bindings, checks the
license, shows the account's skins and capes, downloads the active skin,
and launches a version online with it until a log message contains a
marker.

Tokens go to a Python SecretStore that keeps them in memory; a real
launcher would back it with the system keychain.

    cargo build -p coblet-uniffi
    cargo run -p coblet-uniffi --features cli --bin uniffi-bindgen -- \
        generate --library target/debug/libcoblet_uniffi.dylib \
        --language python --out-dir target/uniffi-py
    cp target/debug/libcoblet_uniffi.dylib target/uniffi-py/
    COBLET_MSA_CLIENT_ID=<your client id> PYTHONPATH=target/uniffi-py \
        python3 examples/accounts.py <data root> <version> <marker> [--browser]
"""

import asyncio
import os
import sys

import coblet_uniffi as coblet


class MemoryKeychain(coblet.SecretStore):
    def __init__(self) -> None:
        self.values: dict[str, str] = {}

    def get(self, key: str) -> str | None:
        return self.values.get(key)

    def put(self, key: str, value: str) -> None:
        self.values[key] = value

    def delete(self, key: str) -> None:
        self.values.pop(key, None)


async def main(data_root: str, version: str, marker: str, browser: bool) -> int:
    keychain = MemoryKeychain()
    builder = coblet.CobletBuilder(data_root, "coblet-uniffi-accounts/0.1")
    builder.set_msa_client_id(os.environ["COBLET_MSA_CLIENT_ID"])
    builder.set_secret_store(keychain)
    launcher = builder.build()

    interaction = (
        coblet.LoginInteraction.BROWSER() if browser else coblet.LoginInteraction.DEVICE_CODE()
    )
    flow = await launcher.begin_microsoft_login(interaction, None)
    prompt = flow.prompt()
    if prompt.is_device_code():
        print(f"open {prompt.verification_uri} and enter {prompt.user_code}")
    elif prompt.is_open_url():
        print(f"open this page and sign in:\n{prompt.url}")
    account = await flow.finish()
    print(f"signed in: {account.id} {account.state} {account.player and account.player.name}")
    print(f"secrets kept: {sorted(key.rsplit('/', 1)[-1] for key in keychain.values)}")

    account = await launcher.verify_license(account.id)
    print(f"license: {account.license} owns the game: {account.owns_game}")

    account = await launcher.refresh_profile(account.id)
    player = account.player
    for skin in player.skins:
        print(f"skin {'*' if skin.active else ' '} {skin.model} {skin.url}")
    for cape in player.capes:
        print(f"cape {'*' if cape.active else ' '} {cape.id} {cape.alias}")
    active = next((skin for skin in player.skins if skin.active), None)
    if active is not None:
        png = await launcher.download_texture(active.url)
        print(f"active skin texture: {len(png)} bytes")

    game = await launcher.launch_account(version, account.id, None, [])
    print(f"started pid {game.pid()}")
    reached = False
    while (line := await game.next_log()) is not None:
        print(f"[{line.source} {line.level}] {line.message}")
        if not reached and marker in line.message:
            print("reached marker, killing the game")
            reached = True
            game.kill()
    print(f"exit: {await game.wait()}")
    return 0 if reached else 1


if __name__ == "__main__":
    args = [arg for arg in sys.argv[1:] if arg != "--browser"]
    if len(args) != 3:
        print(__doc__)
        sys.exit(2)
    sys.exit(asyncio.run(main(*args, browser="--browser" in sys.argv)))
