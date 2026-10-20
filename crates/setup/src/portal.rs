//! Serving the setup form with edge-http.
//!
//! - Captive mode (the device's own access point): every GET returns the form, which is
//!   what makes phones and laptops pop it up after joining.
//! - LAN mode: only `/setup`; [`WithSetup`] puts it in front of another handler (IPP).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{Debug, Display};

use edge_http::Method;
use edge_http::io::Error;
use edge_http::io::server::{Connection, Handler};
use edge_nal::TcpSplit;
use embedded_io_async::{Read, Write};

use crate::Settings;
use crate::form::{SETUP_PATH, render_form, render_saved, settings_from_form};

/// Largest form body accepted.
const MAX_BODY: usize = 2048;

/// What the portal needs from the device.
pub trait SetupBackend {
    /// Printer name, for page titles.
    fn name(&self) -> String;
    fn current(&self) -> Settings;
    /// Wi-Fi networks to offer, strongest first.
    fn networks(&self) -> Vec<String>;
    /// Stores the settings. Returns whether the device restarts to apply them (after the
    /// response is sent); otherwise they were applied in place. Errors are shown to the user.
    fn save(&self, settings: &Settings) -> Result<Restart, &'static str>;
}

/// Whether saved settings need a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    Yes,
    No,
}

pub struct SetupHandler<B> {
    backend: B,
    captive: bool,
}

impl<B: SetupBackend> SetupHandler<B> {
    /// Captive portal: answers every path.
    pub fn captive(backend: B) -> Self {
        Self { backend, captive: true }
    }

    /// LAN mode: answers [`SETUP_PATH`] only.
    pub fn lan(backend: B) -> Self {
        Self { backend, captive: false }
    }

    fn form(&self, error: Option<&str>) -> String {
        render_form(&self.backend.name(), &self.backend.current(), &self.backend.networks(), error)
    }

    async fn submit<T: Read + Write, const N: usize>(&self, connection: &mut Connection<'_, T, N>) -> Result<(), Error<T::Error>> {
        let mut body = Vec::new();
        let mut chunk = [0u8; 256];
        loop {
            let n = connection.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            if body.len() + n > MAX_BODY {
                return html(connection, 413, &self.form(Some("The form was too large."))).await;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&body);
        let current = self.backend.current();
        let result = settings_from_form(&body, &current).and_then(|s| self.backend.save(&s).map(|r| (s, r)));
        match result {
            Ok((saved, restart)) => {
                log::info!(
                    "setup: saved settings for network {:?}, location {:?}, brightness {}",
                    saved.ssid,
                    saved.location,
                    saved.brightness
                );
                html(connection, 200, &render_saved(&self.backend.name(), &saved, restart == Restart::Yes)).await
            }
            Err(message) => html(connection, 400, &self.form(Some(message))).await,
        }
    }
}

impl<B: SetupBackend> Handler for SetupHandler<B> {
    type Error<E: Debug> = Error<E>;

    async fn handle<T, const N: usize>(
        &self,
        _task_id: impl Display + Copy,
        connection: &mut Connection<'_, T, N>,
    ) -> Result<(), Self::Error<T::Error>>
    where
        T: Read + Write + TcpSplit,
    {
        let headers = connection.headers()?;
        let method = headers.method;
        let path = headers.path.split('?').next().unwrap_or("");
        let is_setup = path.trim_end_matches('/') == SETUP_PATH;
        match method {
            Method::Post if is_setup => self.submit(connection).await,
            Method::Get if is_setup || self.captive => html(connection, 200, &self.form(None)).await,
            _ => html(connection, 404, "not found").await,
        }
    }
}

async fn html<T: Read + Write, const N: usize>(
    connection: &mut Connection<'_, T, N>,
    status: u16,
    body: &str,
) -> Result<(), Error<T::Error>> {
    let len = format!("{}", body.len());
    connection
        .initiate_response(
            status,
            None,
            &[("Content-Type", "text/html; charset=utf-8"), ("Content-Length", &len), ("Cache-Control", "no-store")],
        )
        .await?;
    connection.write_all(body.as_bytes()).await
}

/// Routes [`SETUP_PATH`] to a LAN-mode [`SetupHandler`] and everything else to `other`.
pub struct WithSetup<B, H> {
    pub setup: SetupHandler<B>,
    pub other: H,
}

#[derive(Debug)]
pub enum RouteError<A, B> {
    Setup(A),
    Other(B),
}

impl<B: SetupBackend, H: Handler> Handler for WithSetup<B, H> {
    type Error<E: Debug> = RouteError<Error<E>, H::Error<E>>;

    async fn handle<T, const N: usize>(
        &self,
        task_id: impl Display + Copy,
        connection: &mut Connection<'_, T, N>,
    ) -> Result<(), Self::Error<T::Error>>
    where
        T: Read + Write + TcpSplit,
    {
        let path = connection.headers().map_err(RouteError::Setup)?.path;
        if path.split('?').next().unwrap_or("").trim_end_matches('/') == SETUP_PATH {
            self.setup.handle(task_id, connection).await.map_err(RouteError::Setup)
        } else {
            self.other.handle(task_id, connection).await.map_err(RouteError::Other)
        }
    }
}
