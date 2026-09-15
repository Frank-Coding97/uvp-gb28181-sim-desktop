//! Windows Media Foundation capture backend.
//!
//! This module is intentionally introduced separately from the legacy
//! DirectShow path so device opening and frame transport can be validated
//! independently before wiring it into live capture.

#![cfg(windows)]

use common::{Error, Result};

pub(super) fn unavailable_until_runtime_probe() -> Result<()> {
    Err(Error::Media(
        "Windows Media Foundation backend is present but not wired into live capture yet".into(),
    ))
}
