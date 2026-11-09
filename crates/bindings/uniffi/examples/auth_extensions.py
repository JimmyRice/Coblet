"""Auth extensions from Python: sign in with a login server written in
Python, launch with the session it returns, and change skins through an
authlib-injector style appearance service described in TOML.

The example starts its own login and skin server on 127.0.0.1, so no real
account or site is involved. The auth method signs in through
`ctx.send` with a password passed as a login secret, keeps the server
address in the account's data and rotates its token on refresh; its
sessions add a JVM argument, which the example reads back from the game's
command line where `ps` exists. Tokens are never printed.

    cargo build -p coblet-uniffi
    cargo run -p coblet-uniffi --features cli --bin uniffi-bindgen -- \
        generate --library target/debug/libcoblet_uniffi.dylib \
        --language python --out-dir target/uniffi-py
    cp target/debug/libcoblet_uniffi.dylib target/uniffi-py/
    PYTHONPATH=target/uniffi-py python3 examples/auth_extensions.py <data root> <version> <marker>
"""

import asyncio
import base64
import hashlib
import json
import secrets
import shutil
import struct
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import coblet_uniffi as coblet

UUID = "0f5c5f6b8a7d4e2fb1d3a8c9e0f1a2b3"
NAME = "Alex"
PASSWORD = "correct horse"


class Site:
    """State of the local login and skin server."""

    def __init__(self) -> None:
        self.tokens: set[str] = set()
        self.skin: tuple[bytes, bool] | None = None
        self.root = ""

    def issue(self) -> str:
        token = secrets.token_hex(16)
        self.tokens.add(token)
        return token


def handler(site: Site) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args) -> None:
            pass

        def reply(self, status: int, body: bytes = b"", kind: str = "application/json") -> None:
            self.send_response(status)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def body(self) -> bytes:
            return self.rfile.read(int(self.headers.get("Content-Length", "0")))

        def authorized(self) -> bool:
            token = self.headers.get("Authorization", "").removeprefix("Bearer ")
            return token in site.tokens

        def do_POST(self) -> None:
            request = json.loads(self.body() or b"{}")
            if self.path == "/authserver/authenticate":
                if request.get("password") != PASSWORD:
                    return self.reply(403, b'{"error":"ForbiddenOperationException"}')
            elif self.path == "/authserver/refresh":
                if request.get("accessToken") not in site.tokens:
                    return self.reply(403, b'{"error":"ForbiddenOperationException"}')
                site.tokens.discard(request["accessToken"])
            else:
                return self.reply(404)
            answer = {
                "accessToken": site.issue(),
                "selectedProfile": {"id": UUID, "name": NAME},
            }
            self.reply(200, json.dumps(answer).encode())

        def do_GET(self) -> None:
            if self.path == f"/sessionserver/session/minecraft/profile/{UUID}":
                textures = {}
                if site.skin is not None:
                    png, slim = site.skin
                    skin = {"url": f"{site.root}/textures/{hashlib.sha256(png).hexdigest()}"}
                    if slim:
                        skin["metadata"] = {"model": "slim"}
                    textures["SKIN"] = skin
                value = base64.b64encode(json.dumps({"textures": textures}).encode()).decode()
                profile = {
                    "id": UUID,
                    "name": NAME,
                    "properties": [
                        {"name": "textures", "value": value},
                        {"name": "uploadableTextures", "value": "skin"},
                    ],
                }
                return self.reply(200, json.dumps(profile).encode())
            if self.path.startswith("/textures/") and site.skin is not None:
                return self.reply(200, site.skin[0], "image/png")
            self.reply(404)

        def do_PUT(self) -> None:
            if self.path != f"/api/user/profile/{UUID}/skin" or not self.authorized():
                return self.reply(401)
            boundary = self.headers["Content-Type"].split("boundary=")[1].encode()
            fields = {}
            for part in self.body().split(b"--" + boundary):
                head, _, value = part.partition(b"\r\n\r\n")
                if b'name="' in head:
                    name = head.split(b'name="')[1].split(b'"')[0].decode()
                    fields[name] = value.removesuffix(b"\r\n")
            site.skin = (fields["file"], fields.get("model") == b"slim")
            self.reply(204)

        def do_DELETE(self) -> None:
            if self.path != f"/api/user/profile/{UUID}/skin" or not self.authorized():
                return self.reply(401)
            site.skin = None
            self.reply(204)

    return Handler


def post(ctx, url: str, body: dict) -> dict:
    """Posts JSON through coblet's transport and reads the JSON answer."""
    request = coblet.HttpRequest(
        method="POST",
        url=url,
        headers=[coblet.HttpHeader(name="Content-Type", value="application/json")],
        body=json.dumps(body).encode(),
    )
    response = ctx.send(request)
    if response.status != 200:
        raise coblet.CobletError.Failed(coblet.ErrorKind.AUTH, f"login server said {response.status}")
    return json.loads(response.body)


def change(server: str, token: str) -> coblet.AccountChange:
    return coblet.AccountChange(
        player=coblet.PlayerInfo(name=NAME, uuid=UUID, skins=[], capes=[]),
        state=coblet.AccountState.READY,
        session_expires_at=None,
        data={"server": server},
        secrets={"access_token": token},
    )


class AcmeLogin(coblet.AuthMethod):
    """Signs in to the local server with a password."""

    def id(self) -> str:
        return "acme:login"

    def display_name(self) -> str:
        return "Acme"

    def appearance_service(self) -> str | None:
        return "acme:skins"

    def login(self, ctx: coblet.LoginContext, request: coblet.LoginRequest) -> coblet.AccountChange:
        server = request.params["server"]
        answer = post(
            ctx,
            f"{server}/authserver/authenticate",
            {"username": request.login_hint, "password": request.secrets["password"]},
        )
        return change(server, answer["accessToken"])

    def refresh(
        self, ctx: coblet.RefreshContext, account: coblet.AccountInfo, mode: coblet.RefreshMode
    ) -> coblet.RefreshResult:
        server = account.data["server"]
        token = ctx.secret("access_token")
        if mode == coblet.RefreshMode.FORCE or token is None:
            token = post(ctx, f"{server}/authserver/refresh", {"accessToken": token})["accessToken"]
            ctx.put_secret("access_token", token)
        session = coblet.SessionInfo(
            player_name=NAME,
            uuid=UUID,
            access_token=token,
            xuid=None,
            client_id=None,
            user_type="mojang",
            jvm_args=[f"-Dacme.auth={server}"],
            expires_at=None,
        )
        return coblet.RefreshResult(change=change(server, token), session=session)


def skin_png() -> bytes:
    """Returns the header of a 64x64 PNG, enough for the skin checks."""
    header = struct.pack(">II", 64, 64) + bytes([8, 6, 0, 0, 0])
    return b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + header + b"\0" * 4


def print_command(pid: int | None) -> None:
    if pid is None or shutil.which("ps") is None:
        return
    args = subprocess.run(
        ["ps", "-ww", "-o", "args=", "-p", str(pid)], capture_output=True, text=True
    ).stdout.split()
    for arg in args:
        if arg.startswith("-Dacme."):
            print(f"command: {arg}")


async def main(data_root: str, version: str, marker: str) -> int:
    site = Site()
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler(site))
    site.root = f"http://127.0.0.1:{server.server_address[1]}"
    threading.Thread(target=server.serve_forever, daemon=True).start()

    builder = coblet.CobletBuilder(data_root, "coblet-uniffi-auth-extensions/0.1")
    builder.add_auth_method(AcmeLogin())
    builder.add_appearance_service_toml(
        f'id = "acme:skins"\nname = "Acme skins"\nkind = "yggdrasil"\napi_root = "{site.root}"\n'
    )
    launcher = builder.build()
    print(f"auth methods: {[method.id for method in launcher.auth_methods()]}")
    print(f"appearance services: {[service.id for service in launcher.appearance_services()]}")

    flow = await launcher.begin_login(
        "acme:login",
        coblet.LoginInteraction.DEVICE_CODE(),
        "alex@acme.test",
        {"server": site.root},
        {"password": PASSWORD},
    )
    print(f"prompt: {type(flow.prompt()).__name__}")
    account = await flow.finish()
    print(f"signed in: {account.player.name} via {account.method}, data {account.data}")

    caps = await launcher.appearance_capabilities(account.id)
    print(f"capabilities: upload_png={caps.upload_png} set_model={caps.set_model} "
          f"reset_skin={caps.reset_skin} choose_cape={caps.choose_cape}")
    worn = await launcher.upload_skin(account.id, skin_png(), coblet.SkinModel.CLASSIC)
    print(f"uploaded: {worn.skins[0].model}")
    worn = await launcher.set_skin_model(account.id, coblet.SkinModel.SLIM)
    print(f"model now: {worn.skins[0].model}")
    worn = await launcher.reset_skin(account.id)
    print(f"skins after reset: {len(worn.skins)}")
    try:
        await launcher.set_cape(account.id, "migrator")
    except coblet.CobletError.Failed as error:
        print(f"set cape: {error.kind}")

    game = await launcher.launch_account(version, account.id, None, [])
    print(f"started pid {game.pid()}")
    reached = False
    while (line := await game.next_log()) is not None:
        if not reached and marker in line.message:
            print(f"reached marker: {line.message}")
            reached = True
            print_command(game.pid())
            game.kill()
    print(f"exit: {await game.wait()}")
    server.shutdown()
    return 0 if reached else 1


if __name__ == "__main__":
    if len(sys.argv) != 4:
        print(__doc__)
        sys.exit(2)
    sys.exit(asyncio.run(main(*sys.argv[1:])))
