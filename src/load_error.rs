//! Classifying a failed [`crate::emulator::Emulator::load`] into something the
//! user can act on.
//!
//! `load` returns `anyhow::Result`, which keeps every `?` and `.context()` call
//! site cheap but leaves the caller with only a message to show. Since anyhow
//! preserves the whole source chain, the concrete error is still in there — so
//! rather than change the load path to a typed error end to end, the display
//! boundary walks the chain and downcasts. [`classify`] is the one place that
//! knows how each failure looks, so the HUD, the log and any future caller
//! agree on what counts as "not found" or "offline".
//!
//! Failures raised by demarc itself (no core, missing BIOS, unknown system)
//! have nothing to downcast to unless they are given a type, hence the small
//! error structs below — a `bail!("...")` string would only be matchable by
//! substring, which breaks the moment someone rewords the message.

use crate::systems::SystemType;

/// No libretro core is installed for the system this file needs.
#[derive(Debug, thiserror::Error)]
#[error("no libretro core '{name}' for {system:?}")]
pub struct NoCore {
    pub name: String,
    pub system: SystemType,
}

/// A core was found but needs a BIOS the user hasn't supplied.
#[derive(Debug, thiserror::Error)]
#[error("{core} needs a BIOS: put one of {} in {dir}{}",
        .candidates.join(", "),
        .note.as_ref().map(|n| format!(". {n}")).unwrap_or_default())]
pub struct MissingBios {
    pub core: String,
    /// Accepted BIOS filenames, any one of which satisfies the core.
    pub candidates: Vec<String>,
    pub dir: String,
    /// Optional extra explanation of why this core in particular is required.
    pub note: Option<String>,
}

/// The file's system type couldn't be determined, so there is no core to pick.
#[derive(Debug, thiserror::Error)]
#[error("unsupported or unrecognized file type")]
pub struct UnknownSystem;

/// What went wrong, at the granularity worth telling the user about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadFailure {
    /// The server answered, but the file isn't there (HTTP 4xx).
    NotFound,
    /// The server accepted a connection or DNS lookup but never answered in
    /// time — see the timeouts in [`crate::fetch`].
    Timeout,
    /// Couldn't reach the host at all: DNS failure, refused connection, no
    /// route. Usually means the mirror is down or there's no network.
    Offline,
    /// Reached the server, but the transfer itself failed (protocol error,
    /// truncated body, 5xx).
    DownloadFailed,
    NoCore,
    MissingBios,
    UnknownSystem,
    /// Anything else — unpacking, conversion, core initialization.
    Other,
}

impl LoadFailure {
    /// Short reason to append to the HUD's "Could not load X" line. Kept to a
    /// few words because the HUD is one line over the emulator output.
    pub fn reason(self) -> &'static str {
        match self {
            LoadFailure::NotFound => "404: not found",
            LoadFailure::Timeout => "server timed out",
            LoadFailure::Offline => "cannot reach server",
            LoadFailure::DownloadFailed => "download failed",
            LoadFailure::NoCore => "no emulator core",
            LoadFailure::MissingBios => "missing BIOS",
            LoadFailure::UnknownSystem => "unsupported file type",
            LoadFailure::Other => "load failed",
        }
    }
}

/// Classify a `load` error by walking its source chain for a known concrete
/// error type. Ordering matters only in that demarc's own errors are checked
/// first; a single failure never carries two of these.
pub fn classify(error: &anyhow::Error) -> LoadFailure {
    if error.chain().any(|e| e.is::<NoCore>()) {
        return LoadFailure::NoCore;
    }
    if error.chain().any(|e| e.is::<MissingBios>()) {
        return LoadFailure::MissingBios;
    }
    if error.chain().any(|e| e.is::<UnknownSystem>()) {
        return LoadFailure::UnknownSystem;
    }
    if let Some(e) = error.chain().find_map(|e| e.downcast_ref::<ureq::Error>()) {
        return classify_ureq(e);
    }
    if let Some(e) = error
        .chain()
        .find_map(|e| e.downcast_ref::<suppaftp::FtpError>())
    {
        return classify_ftp(e);
    }
    LoadFailure::Other
}

fn classify_ureq(error: &ureq::Error) -> LoadFailure {
    match error {
        // 404 and friends mean the link is stale; 5xx means the mirror is
        // broken, which is a failed download rather than a missing file.
        ureq::Error::StatusCode(code) => {
            if (400..500).contains(code) {
                LoadFailure::NotFound
            } else {
                LoadFailure::DownloadFailed
            }
        }
        ureq::Error::Timeout(_) => LoadFailure::Timeout,
        // A failed DNS lookup arrives as `Io` with an uncategorized kind rather
        // than the `HostNotFound` variant you'd expect, so both land here — and
        // so does a refused connection or a mid-transfer reset.
        ureq::Error::HostNotFound => LoadFailure::Offline,
        ureq::Error::Io(e) => classify_io(e),
        ureq::Error::ConnectionFailed => LoadFailure::Offline,
        _ => LoadFailure::DownloadFailed,
    }
}

fn classify_ftp(error: &suppaftp::FtpError) -> LoadFailure {
    match error {
        suppaftp::FtpError::ConnectionError(e) => classify_io(e),
        // 550 and the other 5xx replies to RETR mean the path isn't there.
        suppaftp::FtpError::UnexpectedResponse(r) if r.status.code() >= 500 => {
            LoadFailure::NotFound
        }
        _ => LoadFailure::DownloadFailed,
    }
}

fn classify_io(error: &std::io::Error) -> LoadFailure {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => LoadFailure::Timeout,
        ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::NotConnected
        | ErrorKind::HostUnreachable
        | ErrorKind::NetworkUnreachable
        | ErrorKind::NetworkDown => LoadFailure::Offline,
        // DNS failure has no dedicated `ErrorKind` and surfaces as
        // `Uncategorized`, which is unstable to match on by name; recognize it
        // by message instead. Anything else is a genuine transfer error.
        _ => {
            let msg = error.to_string();
            if msg.contains("lookup address") || msg.contains("Name or service not known") {
                LoadFailure::Offline
            } else {
                LoadFailure::DownloadFailed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chain is walked, not just the outermost error: real failures are
    /// always wrapped in at least one `.context()` by the time they get here.
    #[test]
    fn classifies_through_context() {
        let err = anyhow::Error::new(NoCore {
            name: "vice_x64sc".into(),
            system: SystemType::C64,
        })
        .context("could not prepare file")
        .context("load failed");
        assert_eq!(classify(&err), LoadFailure::NoCore);
    }

    #[test]
    fn classifies_http_status() {
        let err = anyhow::Error::new(ureq::Error::StatusCode(404)).context("downloading");
        assert_eq!(classify(&err), LoadFailure::NotFound);
        let err = anyhow::Error::new(ureq::Error::StatusCode(503));
        assert_eq!(classify(&err), LoadFailure::DownloadFailed);
    }

    #[test]
    fn classifies_io_kinds() {
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert_eq!(classify_io(&refused), LoadFailure::Offline);
        let timed_out = std::io::Error::from(std::io::ErrorKind::TimedOut);
        assert_eq!(classify_io(&timed_out), LoadFailure::Timeout);
        // DNS failure, as libc actually reports it.
        let dns = std::io::Error::other("failed to lookup address information: unknown");
        assert_eq!(classify_io(&dns), LoadFailure::Offline);
    }

    #[test]
    fn classifies_ftp_connect_failure() {
        let err = anyhow::Error::new(suppaftp::FtpError::ConnectionError(std::io::Error::from(
            std::io::ErrorKind::TimedOut,
        )))
        .context("failed to connect to ftp.example.org:21");
        assert_eq!(classify(&err), LoadFailure::Timeout);
    }

    /// An error with nothing recognizable in it must not be mislabeled.
    #[test]
    fn unrecognized_errors_fall_through() {
        let err = anyhow::anyhow!("conversion failed");
        assert_eq!(classify(&err), LoadFailure::Other);
    }
}

