//! Manages accounts, skins and capes, and launches with an account.
//!
//! ```text
//! COBLET_MSA_CLIENT_ID=<your client id> cargo run -p coblet --example accounts \
//!     --features auth-loopback,encrypted-secrets -- [--data DIR] <command>
//!
//! commands:
//!   login [--browser [--host H] [--port N] [--path P]] [--redirect URI]
//!   offline NAME
//!   list
//!   default ACCOUNT
//!   remove ACCOUNT
//!   refresh ACCOUNT
//!   appearance ACCOUNT
//!   skin ACCOUNT upload FILE [--slim] | url URL [--slim] | model classic|slim | reset
//!   cape ACCOUNT CAPE-ID|none
//!   launch ACCOUNT VERSION [--run-for SECONDS] [--until TEXT]
//! ```
//!
//! ACCOUNT is an account ID or a player name. The Client ID is read from
//! `COBLET_MSA_CLIENT_ID` at run time; the example never stores it.
//!
//! Tokens go to `DIR/secrets.bin`, encrypted with a key kept in
//! `DIR/secrets.key`. Keeping the key next to the file only suits a demo; a
//! real launcher keeps it in a system keychain.
//!
//! `login` uses the device code flow by default. `--browser` opens a
//! loopback listener and prints the sign-in URL; `--redirect URI` prints
//! the URL and reads the redirected URI from standard input.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the example reports progress on the terminal"
)]

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use coblet::auth::{
    Account, AccountId, EncryptedFileStore, EncryptionKey, Interaction, LoginOptions, MsaClientId,
    PlayerProfile, Redirect, SkinModel,
};
use coblet::launch::LogItem;
use coblet::player::SkinSource;
use coblet::runtime::{AuthPrompt, HttpUrl, UserAgent};
use coblet::types::PlayerName;
use coblet::{Coblet, Error};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn usage() -> String {
    "usage: accounts [--data DIR] login|offline|list|default|remove|refresh|appearance|skin|cape|launch ...; see the file header".to_owned()
}

fn secret_key(data: &Path) -> Result<EncryptionKey> {
    let path = data.join("secrets.key");
    if let Ok(bytes) = std::fs::read(&path) {
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| format!("{} is not a 32 byte key", path.display()))?;
        return Ok(EncryptionKey::from_bytes(bytes));
    }
    let key = EncryptionKey::generate()?;
    std::fs::create_dir_all(data)?;
    std::fs::write(&path, key.as_bytes())?;
    Ok(key)
}

fn build(data: &Path) -> Result<Coblet> {
    let secrets = EncryptedFileStore::new(
        Arc::new(coblet::runtime::TokioStorage::new()),
        data.join("secrets.bin"),
        secret_key(data)?,
    );
    let mut builder = Coblet::builder()
        .data_root(data)
        .user_agent(UserAgent::new(concat!(
            "coblet-example/",
            env!("CARGO_PKG_VERSION")
        ))?)
        .secret_store(Arc::new(secrets));
    if let Ok(id) = std::env::var("COBLET_MSA_CLIENT_ID") {
        builder = builder.msa_client_id(MsaClientId::new(id)?);
    }
    Ok(builder.build()?)
}

fn describe(account: &Account) -> String {
    let name = account
        .player_name()
        .map_or("<no profile>".to_owned(), ToString::to_string);
    format!(
        "{}  {name}  {}  {}",
        account.id, account.method, account.state
    )
}

fn show(profile: &PlayerProfile) {
    println!("{} ({})", profile.name, profile.uuid);
    for skin in &profile.skins {
        let mark = if skin.active { "*" } else { " " };
        println!("  skin {mark} {} {} {}", skin.id, skin.model, skin.url);
    }
    for cape in &profile.capes {
        let mark = if cape.active { "*" } else { " " };
        let alias = cape.alias.as_deref().unwrap_or("?");
        println!("  cape {mark} {} {alias} {}", cape.id, cape.url);
    }
}

async fn find(coblet: &Coblet, wanted: &str) -> Result<AccountId> {
    for account in coblet.accounts().list().await? {
        let by_name = account
            .player_name()
            .is_some_and(|name| name.as_str().eq_ignore_ascii_case(wanted));
        if account.id.as_str() == wanted || by_name {
            return Ok(account.id);
        }
    }
    Err(format!("no account {wanted}").into())
}

fn login_options(args: &[String]) -> Result<LoginOptions> {
    let mut options = LoginOptions::new();
    let mut iter = args.iter();
    let (mut host, mut port, mut path) = ("127.0.0.1".to_owned(), 0_u16, "/".to_owned());
    let mut browser = false;
    while let Some(arg) = iter.next() {
        let mut value = || iter.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--browser" => browser = true,
            "--host" => host = value()?,
            "--port" => port = value()?.parse()?,
            "--path" => path = value()?,
            "--redirect" => {
                options = options.with_interaction(Interaction::Browser(Redirect::External {
                    redirect_uri: value()?,
                }));
            }
            other => return Err(format!("unknown login option {other}").into()),
        }
    }
    if browser {
        options = options.with_interaction(Interaction::Browser(Redirect::Loopback {
            host,
            port,
            path,
        }));
    }
    Ok(options)
}

async fn login(coblet: &Coblet, args: &[String]) -> Result<()> {
    let options = login_options(args)?;
    let flow = coblet.begin_microsoft_login(&options).await?;
    match flow.prompt() {
        AuthPrompt::DeviceCode {
            user_code,
            verification_uri,
            ..
        } => println!("open {verification_uri} and enter {user_code}"),
        AuthPrompt::OpenUrl { url, redirect_uri } => {
            println!("open this page and sign in:\n{url}");
            if let Some(callback) = flow
                .callback()
                .filter(|_| !redirect_uri.starts_with("http"))
            {
                println!("then paste the address you were sent to:");
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line)?;
                callback.submit(line.trim())?;
            }
        }
        other => println!("{other:?}"),
    }
    let account = coblet.finish_microsoft_login(flow).await?;
    println!("signed in: {}", describe(&account));
    Ok(())
}

async fn skin(coblet: &Coblet, account: &AccountId, args: &[String]) -> Result<()> {
    let model = if args.iter().any(|arg| arg == "--slim") {
        SkinModel::Slim
    } else {
        SkinModel::Classic
    };
    let profile = match args.first().map(String::as_str) {
        Some("upload") => {
            let file = args.get(1).ok_or("upload needs a file")?;
            let png = std::fs::read(file)?;
            coblet
                .upload_skin(account, SkinSource::Png(png), model)
                .await?
        }
        Some("url") => {
            let url = HttpUrl::new(args.get(1).ok_or("url needs a url")?.as_str())?;
            coblet
                .upload_skin(account, SkinSource::Url(url), model)
                .await?
        }
        Some("model") => {
            let model = args
                .get(1)
                .and_then(|name| SkinModel::parse(name))
                .ok_or("model needs classic or slim")?;
            coblet.set_skin_model(account, model).await?
        }
        Some("reset") => coblet.reset_skin(account).await?,
        _ => return Err(usage().into()),
    };
    show(&profile);
    Ok(())
}

async fn launch(coblet: &Coblet, account: &AccountId, args: &[String]) -> Result<bool> {
    let version_id = args.first().ok_or("launch needs a version")?;
    let mut run_for = None;
    let mut until = None;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let mut value = || iter.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--run-for" => run_for = Some(Duration::from_secs(value()?.parse()?)),
            "--until" => until = Some(value()?),
            other => return Err(format!("unknown launch option {other}").into()),
        }
    }
    let request = coblet.launch_request(account).await?;
    let game = coblet.data_root().join("games").join(version_id);
    let session = coblet.launch(version_id, game, request).await?;
    println!("== started pid {:?}: {}", session.pid(), session.command());
    let mut logs = session.logs();
    let deadline = tokio::time::Instant::now() + run_for.unwrap_or(Duration::from_mins(10));
    let mut reached = false;
    loop {
        let Ok(item) = tokio::time::timeout_at(deadline, logs.next()).await else {
            reached = run_for.is_some();
            session.kill();
            break;
        };
        match item {
            Some(LogItem::Line(line)) => {
                println!("[{:?} {}] {}", line.source, line.level, line.message);
                if until
                    .as_deref()
                    .is_some_and(|text| line.message.contains(text))
                {
                    reached = true;
                    session.kill();
                }
            }
            Some(LogItem::Missed(count)) => println!("== missed {count} lines"),
            None => break,
        }
    }
    let exit = session.wait().await?;
    println!(
        "== exit {} after {:.1}s",
        exit.exit,
        exit.duration.as_secs_f64()
    );
    Ok(reached || (until.is_none() && run_for.is_none() && !exit.suspected_crash))
}

async fn run(args: Vec<String>) -> Result<bool> {
    let mut args = args;
    let mut data = PathBuf::from("coblet-data");
    if args.first().map(String::as_str) == Some("--data") {
        data = args.get(1).ok_or("--data needs a value")?.into();
        args.drain(..2);
    }
    let coblet = build(&data)?;
    let command = args.first().cloned().ok_or_else(usage)?;
    let rest = &args[1..];
    let account = || async {
        let wanted = rest.first().ok_or_else(usage)?;
        find(&coblet, wanted).await
    };
    match command.as_str() {
        "login" => login(&coblet, rest).await?,
        "offline" => {
            let name = PlayerName::new(rest.first().ok_or_else(usage)?.as_str())?;
            let account = coblet.accounts().add_offline(name).await?;
            println!("added: {}", describe(&account));
        }
        "list" => {
            let default = coblet.accounts().default_account().await?.map(|a| a.id);
            for account in coblet.accounts().list().await? {
                let mark = if Some(&account.id) == default.as_ref() {
                    "*"
                } else {
                    " "
                };
                println!("{mark} {}", describe(&account));
            }
        }
        "default" => {
            coblet
                .accounts()
                .set_default(Some(&account().await?))
                .await?;
        }
        "remove" => coblet.accounts().remove(&account().await?).await?,
        "refresh" => {
            let account = coblet.accounts().refresh(&account().await?).await?;
            println!("refreshed: {}", describe(&account));
        }
        "appearance" => show(&coblet.appearance(&account().await?).await?),
        "skin" => skin(&coblet, &account().await?, &rest[1..]).await?,
        "cape" => {
            let cape = rest.get(1).ok_or_else(usage)?;
            let cape = (cape != "none").then_some(cape.as_str());
            show(&coblet.set_cape(&account().await?, cape).await?);
        }
        "launch" => return launch(&coblet, &account().await?, &rest[1..]).await,
        _ => return Err(usage().into()),
    }
    Ok(true)
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            if let Some(Error::Auth(auth)) = error.downcast_ref::<Error>() {
                eprintln!("error: {auth} ({auth:?})");
            } else {
                eprintln!("error: {error}");
            }
            ExitCode::FAILURE
        }
    }
}
