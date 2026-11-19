//! `diagnostics_package()` over FFI (spec/07 "Queries", spec/10
//! "Privacy"; plan M2.7c4): the package's text, which the shell
//! previews and writes only where the user chooses. The core writes no
//! file.

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::CancelToken;
use crate::error::CoreError;

#[uniffi::export]
impl Core {
    /// `diagnostics_package()` (spec/07 "Queries"): the one UTF-8 JSON
    /// document. The app and OS versions come from the shell, since the
    /// core knows only its own. A query: the token races the core's
    /// read lock and the work itself.
    pub async fn diagnostics_package(
        &self,
        token: Arc<CancelToken>,
        app_version: String,
        os_version: String,
    ) -> Result<String, CoreError> {
        self.query(&token, async |core| {
            core.diagnostics_package(&app_version, &os_version).await
        })
        .await
    }
}

#[cfg(test)]
mod tests;
