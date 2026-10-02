//! Signature and digest policy for confirming unknown plugin publishers.

use super::tr_package::{self, TrPackageSignatureStatus};
use crate::core::app::error::{Result, TingError};
use crate::core::security::signing::constant_time_eq;
use crate::plugin::types::PluginMetadata;
use crate::plugin::types::metadata::parse_plugin_metadata_content;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path as FsPath;

/// Manifest and package identity that the user must review before installation.
/// This value has no HTTP or presentation concerns.
pub(crate) struct PendingInstallConfirmation {
    pub signature_status: TrPackageSignatureStatus,
    pub metadata: PluginMetadata,
    pub package_sha256: String,
    pub package_changed: bool,
}

/// Inspect the actual package and require acceptance of this exact digest.
///
/// Trusted signatures do not need confirmation. Invalid and unsigned packages
/// remain errors, including when the client supplies blanket acceptance.
pub(crate) fn inspect_install_confirmation(
    package_path: &FsPath,
    accept_unverified: bool,
    confirmed_package_sha256: Option<&str>,
) -> Result<Option<PendingInstallConfirmation>> {
    if !tr_package::has_tr_magic(package_path)? {
        return Ok(None);
    }

    let signature_status = tr_package::verify_tr_package_signature(package_path)?;
    if let TrPackageSignatureStatus::Invalid { reason, .. } = &signature_status {
        return Err(TingError::PluginLoadError(format!(
            "Invalid plugin package signature: {}",
            reason
        )));
    }
    if matches!(signature_status, TrPackageSignatureStatus::Unsigned) {
        return Err(TingError::PluginLoadError(
            "Plugin package is not signed; please build it with trpack".to_string(),
        ));
    }
    if !signature_status.is_installable_with_confirmation() {
        return Ok(None);
    }

    let package_sha256 = plugin_package_sha256(package_path)?;
    if package_confirmation_matches(accept_unverified, confirmed_package_sha256, &package_sha256) {
        return Ok(None);
    }
    let metadata_content = tr_package::read_manifest_file(package_path, "plugin.yml")?;
    let metadata = parse_plugin_metadata_content(&metadata_content, "plugin.yml")?;

    Ok(Some(PendingInstallConfirmation {
        signature_status,
        metadata,
        package_sha256,
        package_changed: accept_unverified,
    }))
}

pub(super) fn package_confirmation_matches(
    accepted: bool,
    confirmed: Option<&str>,
    actual: &str,
) -> bool {
    accepted
        && confirmed.is_some_and(|digest| constant_time_eq(digest.as_bytes(), actual.as_bytes()))
}

pub(super) fn plugin_package_sha256(path: &FsPath) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
