use std::ffi::OsString;

use coblet_launch_command::{JavaExecutable, LaunchAuth, LaunchCommand, MainClass};
use coblet_types::PlayerName;

use super::process_spec;

#[test]
fn wraps_the_java_command() {
    let command = LaunchCommand::builder()
        .java(JavaExecutable::new("/jdk/bin/java"))
        .main_class(MainClass::new("Main"))
        .auth(LaunchAuth::offline(PlayerName::new("Steve").unwrap()))
        .game_directory("/game")
        .build()
        .unwrap();
    let spec = process_spec(
        &command,
        &[OsString::from("prime-run"), OsString::from("-v")],
    );
    assert_eq!(spec.program, std::path::Path::new("prime-run"));
    assert_eq!(spec.args, ["-v", "/jdk/bin/java", "Main"]);
    assert_eq!(spec.cwd.as_deref(), Some(std::path::Path::new("/game")));
}
