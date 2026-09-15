use super::*;

/// The chain is walked, not just the outermost error: real failures are
/// always wrapped in at least one `.context()` by the time they get here.
#[test]
fn classifies_through_context() {
    let err = anyhow::Error::new(NoCore {
        name: "vice_x64sc".into(),
        system: "C64".into(),
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
