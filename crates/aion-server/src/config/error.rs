//! Shared typed configuration-error constructor.

use crate::error::ServerError;

pub(crate) fn config_error<T>(message: impl Into<String>) -> Result<T, ServerError> {
    Err(ServerError::Config {
        message: message.into(),
    })
}
