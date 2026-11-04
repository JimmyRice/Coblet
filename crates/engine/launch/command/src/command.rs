use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::EnvChange;

const REDACTED: &str = "<redacted>";

/// JVM argument file used when the command line would be too long.
///
/// The launcher writes [`content`](ArgFile::content) to [`path`](ArgFile::path)
/// before starting the process; the command passes `@<path>` instead of the
/// JVM arguments and main class. Needs Java 9 or later.
#[derive(Clone, PartialEq, Eq)]
pub struct ArgFile {
    /// Where the file must be written.
    pub path: PathBuf,
    /// File content, one quoted argument per line.
    pub content: String,
}

/// Finished launch command: program, arguments, environment and working directory.
///
/// `Display` and `Debug` replace access tokens, XUIDs and client IDs with
/// `<redacted>`; [`LaunchCommand::expose_secrets`] shows them.
#[derive(Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) env: Vec<EnvChange>,
    pub(crate) cwd: PathBuf,
    pub(crate) argfile: Option<ArgFile>,
    pub(crate) secrets: Vec<String>,
}

impl LaunchCommand {
    /// Returns the `java` executable.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Returns the arguments, unredacted.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// Returns the environment changes.
    #[must_use]
    pub fn env(&self) -> &[EnvChange] {
        &self.env
    }

    /// Returns the working directory (the game directory).
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Returns the argument file to write before launch, if one is used.
    #[must_use]
    pub fn argfile(&self) -> Option<&ArgFile> {
        self.argfile.as_ref()
    }

    /// Returns a view whose `Display` shows secrets in clear text.
    #[must_use]
    pub fn expose_secrets(&self) -> Exposed<'_> {
        Exposed(self)
    }

    /// Replaces every secret in `text`.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        redact(&self.secrets, text)
    }

    fn render(&self, f: &mut fmt::Formatter<'_>, secrets: &[String]) -> fmt::Result {
        f.write_str(&quote(&redact(secrets, &self.program.to_string_lossy())))?;
        for arg in &self.args {
            write!(f, " {}", quote(&redact(secrets, &arg.to_string_lossy())))?;
        }
        Ok(())
    }
}

pub(crate) fn redact(secrets: &[String], text: &str) -> String {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(text.to_owned(), |text, secret| {
            text.replace(secret.as_str(), REDACTED)
        })
}

/// Quotes an argument for display like a POSIX shell would need it.
fn quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,${}".contains(c));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// Quotes an argument for a JVM argument file.
pub(crate) fn argfile_quote(arg: &OsStr) -> String {
    let text = arg.to_string_lossy();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

impl fmt::Display for LaunchCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.render(f, &self.secrets)
    }
}

impl fmt::Debug for LaunchCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let args: Vec<String> = self
            .args
            .iter()
            .map(|arg| self.redact(&arg.to_string_lossy()))
            .collect();
        f.debug_struct("LaunchCommand")
            .field("program", &self.program)
            .field("args", &args)
            .field("env", &self.env)
            .field("cwd", &self.cwd)
            .field(
                "argfile",
                &self.argfile.as_ref().map(|file| ArgFileView(self, file)),
            )
            .finish_non_exhaustive()
    }
}

struct ArgFileView<'a>(&'a LaunchCommand, &'a ArgFile);

impl fmt::Debug for ArgFileView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArgFile")
            .field("path", &self.1.path)
            .field("content", &self.0.redact(&self.1.content))
            .finish()
    }
}

impl fmt::Debug for ArgFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArgFile")
            .field("path", &self.path)
            .field("lines", &self.content.lines().count())
            .finish()
    }
}

/// Unredacted view of a [`LaunchCommand`], from [`LaunchCommand::expose_secrets`].
pub struct Exposed<'a>(&'a LaunchCommand);

impl fmt::Display for Exposed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.render(f, &[])
    }
}
