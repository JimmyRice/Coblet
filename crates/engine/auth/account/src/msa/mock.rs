//! Scripted Microsoft, Xbox and Minecraft services for tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use coblet_runtime::{BoxFuture, MemoryBody, Method, Request, Response, Transport, TransportError};

pub(crate) const DEVICE_CODE: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
pub(crate) const TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
pub(crate) const USER_AUTH: &str = "https://user.auth.xboxlive.com/user/authenticate";
pub(crate) const XSTS: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
pub(crate) const LOGIN: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
pub(crate) const ENTITLEMENTS: &str = "https://api.minecraftservices.com/entitlements/license";
pub(crate) const PROFILE: &str = "https://api.minecraftservices.com/minecraft/profile";

pub(crate) const PROFILE_JSON: &str = r#"{
    "id": "069a79f444e94726a5befca90e38aaf5",
    "name": "Notch",
    "skins": [{"id": "s1", "state": "ACTIVE", "url": "http://textures.minecraft.net/texture/a", "variant": "CLASSIC"}],
    "capes": []
}"#;

/// Request seen by the mock.
#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub(crate) url: String,
    pub(crate) body: String,
}

struct Route {
    method: Method,
    prefix: String,
    replies: VecDeque<(u16, String)>,
}

/// Transport that answers from scripted replies. A route with several
/// replies hands them out in order and repeats the last one.
#[derive(Default)]
pub(crate) struct MockServices {
    routes: Mutex<Vec<Route>>,
    seen: Mutex<Vec<Seen>>,
}

impl MockServices {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Adds a reply for requests whose URL starts with `prefix`.
    pub(crate) fn on(&self, method: Method, prefix: &str, status: u16, body: &str) -> &Self {
        let mut routes = self.routes.lock().unwrap();
        if let Some(route) = routes
            .iter_mut()
            .find(|route| route.method == method && route.prefix == prefix)
        {
            route.replies.push_back((status, body.to_owned()));
        } else {
            routes.push(Route {
                method,
                prefix: prefix.to_owned(),
                replies: VecDeque::from([(status, body.to_owned())]),
            });
        }
        self
    }

    /// Scripts every step after the Microsoft token endpoint for an account
    /// that owns the game and has a profile.
    pub(crate) fn xbox_and_minecraft(&self) -> &Self {
        self.on(
            Method::Post,
            USER_AUTH,
            200,
            r#"{"Token": "xbl-token", "DisplayClaims": {"xui": [{"uhs": "uhs1"}]}}"#,
        )
        .on(
            Method::Post,
            XSTS,
            200,
            r#"{"Token": "xsts-token", "DisplayClaims": {"xui": [{"uhs": "uhs1"}]}}"#,
        )
        .on(
            Method::Post,
            LOGIN,
            200,
            &format!(
                r#"{{"access_token": "{}", "expires_in": 86400, "token_type": "Bearer"}}"#,
                minecraft_token()
            ),
        )
        .on(
            Method::Get,
            ENTITLEMENTS,
            200,
            r#"{"items": [{"name": "product_minecraft"}, {"name": "game_minecraft"}]}"#,
        )
        .on(Method::Get, PROFILE, 200, PROFILE_JSON)
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub(crate) fn seen_at(&self, prefix: &str) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|seen| seen.url.starts_with(prefix))
            .collect()
    }
}

impl Transport for MockServices {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        let url = request.url().as_str().to_owned();
        let body = request
            .body_bytes()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        self.seen.lock().unwrap().push(Seen {
            url: url.clone(),
            body,
        });
        let reply = {
            let mut routes = self.routes.lock().unwrap();
            routes
                .iter_mut()
                .find(|route| route.method == request.method() && url.starts_with(&route.prefix))
                .map(|route| {
                    if route.replies.len() > 1 {
                        route.replies.pop_front().unwrap()
                    } else {
                        route.replies[0].clone()
                    }
                })
        };
        let (status, body) = reply.unwrap_or((418, format!("no route for {url}")));
        Box::pin(async move { Ok(Response::new(status, Vec::new(), MemoryBody::new(body))) })
    }
}

/// Minecraft access token with an `xuid` claim.
pub(crate) fn minecraft_token() -> String {
    let payload = URL_SAFE_NO_PAD.encode(br#"{"xuid":"2535400000000001"}"#);
    format!("eyJhbGciOiJSUzI1NiJ9.{payload}.signature")
}

/// Returns the value of a form field in a recorded body.
pub(crate) fn form_value(body: &str, name: &str) -> Option<String> {
    super::http::query_pairs(&format!("?{body}"))?
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}
