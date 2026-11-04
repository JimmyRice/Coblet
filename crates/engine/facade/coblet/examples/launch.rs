//! Launches a Minecraft version offline or in Demo mode and prints its log.
//!
//! ```text
//! cargo run -p coblet --example launch -- <version|latest> [--demo] [--name NAME]
//!     [--data DIR] [--game DIR] [--java PATH] [--until TEXT] [--run-for SECONDS]
//!     [--timeout SECONDS]
//! ```
//!
//! With `--until`, the game is stopped once a log message contains TEXT;
//! with `--run-for`, once it ran that long. The example then exits with 0.
//! Without either, it waits for the game.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the example reports progress on the terminal"
)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use coblet::Sdk;
use coblet::launch::{LaunchRequest, LogItem};
use coblet::launch_command::LaunchAuth;
use coblet::runtime::UserAgent;
use coblet::types::PlayerName;

struct Args {
    version: String,
    demo: bool,
    name: String,
    data: PathBuf,
    game: Option<PathBuf>,
    java: Option<PathBuf>,
    until: Option<String>,
    run_for: Option<Duration>,
    timeout: Duration,
}

fn seconds(text: &str) -> Result<Duration, String> {
    text.parse()
        .map(Duration::from_secs)
        .map_err(|_| format!("bad number of seconds {text}"))
}

fn parse() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut parsed = Args {
        version: String::new(),
        demo: false,
        name: "Steve".to_owned(),
        data: PathBuf::from("coblet-data"),
        game: None,
        java: None,
        until: None,
        run_for: None,
        timeout: Duration::from_secs(600),
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--demo" => parsed.demo = true,
            "--name" => parsed.name = value()?,
            "--data" => parsed.data = value()?.into(),
            "--game" => parsed.game = Some(value()?.into()),
            "--java" => parsed.java = Some(value()?.into()),
            "--until" => parsed.until = Some(value()?),
            "--run-for" => parsed.run_for = Some(seconds(&value()?)?),
            "--timeout" => parsed.timeout = seconds(&value()?)?,
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            other => other.clone_into(&mut parsed.version),
        }
    }
    if parsed.version.is_empty() {
        return Err("usage: launch <version|latest> [--demo] [--name NAME] [--data DIR] [--game DIR] [--java PATH] [--until TEXT] [--run-for SECONDS] [--timeout SECONDS]".to_owned());
    }
    Ok(parsed)
}

async fn run(args: Args) -> Result<bool, Box<dyn std::error::Error>> {
    let sdk = Sdk::builder()
        .data_root(&args.data)
        .user_agent(UserAgent::new(concat!(
            "coblet-example/",
            env!("CARGO_PKG_VERSION")
        ))?)
        .build()?;
    let version = if args.version == "latest" {
        sdk.latest_release().await?
    } else {
        sdk.version(&args.version).await?
    };
    let profile = sdk.resolve(&version).await?;
    println!(
        "== {} on {} {}",
        version.id,
        sdk.platform().os(),
        sdk.platform().arch()
    );
    println!("{}", profile.explain());
    for warning in &profile.warnings {
        println!("warning: {}", warning.message);
    }
    let name = PlayerName::new(&args.name)?;
    let auth = if args.demo {
        LaunchAuth::demo(name)
    } else {
        LaunchAuth::offline(name)
    };
    let mut request = LaunchRequest::new(auth);
    if let Some(java) = args.java {
        request = request.with_java(java);
    }
    let game = args
        .game
        .unwrap_or_else(|| args.data.join("games").join(version.id.as_str()));
    let session = sdk.launch_profile(profile, game, request).await?;
    println!("== started pid {:?}: {}", session.pid(), session.command());
    let mut logs = session.logs();
    let mut reached = false;
    let deadline = tokio::time::Instant::now() + args.run_for.unwrap_or(args.timeout);
    loop {
        let item = tokio::time::timeout_at(deadline, logs.next()).await;
        let Ok(item) = item else {
            if let Some(run_for) = args.run_for {
                println!(
                    "== still running after {}s, stopping the game",
                    run_for.as_secs()
                );
                reached = true;
            } else {
                eprintln!("== timed out after {}s", args.timeout.as_secs());
            }
            session.kill();
            break;
        };
        match item {
            Some(LogItem::Line(line)) => {
                println!("[{:?} {}] {}", line.source, line.level, line.message);
                if let Some(until) = &args.until
                    && line.message.contains(until.as_str())
                {
                    println!("== reached `{until}`, stopping the game");
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
        "== exit: {} after {:.1}s, killed {}, suspected crash {}, crash reports {:?}",
        exit.exit,
        exit.duration.as_secs_f64(),
        exit.killed,
        exit.suspected_crash,
        exit.crash_reports
    );
    Ok(if args.until.is_some() || args.run_for.is_some() {
        reached
    } else {
        !exit.suspected_crash
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
