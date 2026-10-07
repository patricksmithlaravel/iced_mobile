//! icm's side of the control channel to a running web session: find this
//! project's session record, check it is alive, and send it requests.

use super::server;
use super::viewport::Viewport;
use crate::catalogue::CheckId;
use crate::error::{Evidence, IcmError, Result};
use crate::sessions;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A live session.
#[derive(Clone, Debug)]
pub struct Session {
    /// The session record.
    pub record: Value,
    /// Its port.
    pub port: u16,
    token: String,
    /// The record's path.
    pub path: PathBuf,
}

/// The error for a project without a live web session.
pub fn no_session(detail: impl Into<String>) -> IcmError {
    IcmError::new(CheckId::RunNoSession, detail).fix(
        "Start the app with `icm run web`, then retry.",
        &["icm run web --json -q"],
    )
}

impl Session {
    /// This project's live web session.
    pub fn find(sessions_dir: &Path) -> Result<Session> {
        let path = sessions::path(sessions_dir, "web");
        let Some(record) = sessions::read(sessions_dir, "web") else {
            return Err(no_session(format!(
                "no web session is running for this project ({} does not exist)",
                crate::paths::display(&path)
            )));
        };
        if !sessions::alive(&record) {
            return Err(no_session(format!(
                "the web session recorded in {} (pid {}) is no longer running",
                crate::paths::display(&path),
                sessions::record_pid(&record).unwrap_or(0)
            ))
            .evidence(Evidence::file(&path)));
        }
        Session::from_record(record, path)
    }

    /// A session from its record.
    pub fn from_record(record: Value, path: PathBuf) -> Result<Session> {
        let port = record
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok());
        let token = record
            .get("control")
            .and_then(|c| c.get("token"))
            .and_then(Value::as_str);
        match (port, token) {
            (Some(port), Some(token)) => Ok(Session {
                port,
                token: token.to_string(),
                record,
                path,
            }),
            _ => Err(no_session(format!(
                "{} is not a web session record",
                crate::paths::display(&path)
            ))),
        }
    }

    /// The session's pid.
    pub fn pid(&self) -> i32 {
        sessions::record_pid(&self.record).unwrap_or(0)
    }

    /// The page URL.
    pub fn url(&self) -> String {
        self.record
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// The live console file.
    pub fn console(&self) -> Option<PathBuf> {
        self.record
            .get("logs")
            .and_then(|logs| logs.get("console"))
            .and_then(Value::as_str)
            .map(PathBuf::from)
    }

    /// Sends a control request and returns the reply; a refused or failed
    /// request is an error (`run.no_session` when nothing answers,
    /// `web.chrome_failed` when Chrome could not do it).
    pub fn call(&self, request: Value, timeout: Duration) -> Result<Value> {
        let body = serde_json::to_vec(&request).unwrap_or_default();
        let op = request
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let (status, reply) = server::request(
            self.port,
            "POST",
            server::CONTROL_PATH,
            &[(server::TOKEN_HEADER, &self.token)],
            &body,
            timeout,
        )
        .map_err(|error| {
            no_session(format!(
                "the web session (pid {}) did not answer on port {}: {error}",
                self.pid(),
                self.port
            ))
            .evidence(Evidence::file(&self.path))
        })?;
        let reply: Value = serde_json::from_slice(&reply).unwrap_or(Value::Null);
        match status {
            200 => Ok(reply),
            403 => Err(no_session(format!(
                "port {} is not this project's web session (the token was refused)",
                self.port
            ))),
            _ => {
                let mut error = IcmError::new(
                    CheckId::WebChromeFailed,
                    format!(
                        "the web session could not `{op}`: {}",
                        reply
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("no reason given")
                    ),
                );
                if let Some(log) = self
                    .record
                    .get("logs")
                    .and_then(|l| l.get("chrome"))
                    .and_then(Value::as_str)
                {
                    error = error.evidence(Evidence::file(log));
                }
                Err(error)
            }
        }
    }

    /// The session's status.
    pub fn status(&self, probe: bool) -> Result<Value> {
        self.call(
            json!({"op": "status", "probe": probe}),
            Duration::from_secs(30),
        )
    }

    /// The viewport the session emulates now.
    pub fn viewport(&self) -> Result<Viewport> {
        let status = self.status(false)?;
        Viewport::from_json(&status["viewport"])
            .or_else(|| Viewport::from_json(&self.record["viewport"]))
            .ok_or_else(|| {
                IcmError::new(
                    CheckId::WebChromeFailed,
                    "the web session did not report its viewport",
                )
            })
    }

    /// Writes a screenshot to `out`.
    pub fn screenshot(&self, out: &Path) -> Result<Value> {
        self.call(
            json!({"op": "screenshot", "out": out}),
            Duration::from_secs(60),
        )
    }

    /// Asks the session to end, then makes sure it did.
    pub fn stop(&self) -> sessions::Stopped {
        let _ = self.call(json!({"op": "stop"}), Duration::from_secs(5));
        if sessions::wait_gone(self.pid(), Duration::from_secs(8)) {
            return sessions::Stopped::Terminated;
        }
        sessions::terminate(&self.record, Duration::from_secs(3))
    }
}

/// Stops this project's web session, if one runs; returns its record and
/// how it stopped, and removes the record.
pub fn stop(sessions_dir: &Path) -> Option<(Value, sessions::Stopped)> {
    let path = sessions::path(sessions_dir, "web");
    let record = sessions::read(sessions_dir, "web")?;
    let stopped = if !sessions::alive(&record) {
        sessions::Stopped::NotRunning
    } else {
        match Session::from_record(record.clone(), path.clone()) {
            Ok(session) => session.stop(),
            Err(_) => sessions::terminate(&record, Duration::from_secs(5)),
        }
    };
    if let Some(pid) = sessions::record_pid(&record) {
        sessions::remove(sessions_dir, "web", pid);
    }
    Some((record, stopped))
}
