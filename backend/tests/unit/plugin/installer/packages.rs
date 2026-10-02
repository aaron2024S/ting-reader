use super::*;

pub(crate) fn write_test_signed_package(path: &Path, plugin_manifest: &[u8]) {
    use ed25519_dalek::{Signer, SigningKey};
    let source = b"export function list_plugins() { return []; }";
    let plugin: serde_yaml::Value = serde_yaml::from_slice(plugin_manifest).unwrap();
    let mut files = BTreeMap::from([
        ("plugin.yml", plugin_manifest),
        ("plugin.js", source.as_slice()),
    ]);
    let manifest = PackageManifest {
        format: FORMAT_NAME.into(),
        format_version: FORMAT_VERSION,
        plugin_id: plugin["id"].as_str().unwrap().into(),
        plugin_version: plugin["version"].as_str().unwrap().into(),
        runtime: Some("javascript".into()),
        entry_point: "plugin.js".into(),
        files: files
            .iter()
            .map(|(name, bytes)| PackageFile {
                path: name.to_string(),
                size: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(bytes)),
            })
            .collect(),
    };
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let signature = PackageSignature {
        format: SIGNATURE_FORMAT_NAME.into(),
        format_version: SIGNATURE_FORMAT_VERSION,
        algorithm: SIGNATURE_ALGORITHM.into(),
        key_id: "test-unknown-publisher".into(),
        public_key: hex_lower(key.verifying_key().as_bytes()),
        signature: hex_lower(&key.sign(&signature_payload(&manifest)).to_bytes()),
    };
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let signature_bytes = serde_json::to_vec(&signature).unwrap();
    files.insert(PACKAGE_MANIFEST_PATH, &manifest_bytes);
    files.insert(SIGNATURE_MANIFEST_PATH, &signature_bytes);
    let mut archive = tar::Builder::new(Vec::new());
    for (name, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, name, bytes).unwrap();
    }
    let encoded = zstd::stream::encode_all(archive.into_inner().unwrap().as_slice(), 1).unwrap();
    let mut package = MAGIC.to_vec();
    package.extend(encoded);
    fs::write(path, package).unwrap();
}

mod official_packages {
    use super::*;

    #[test]
    #[ignore = "requires a released store artifact in TING_TEST_PLUGIN_STORE_PACKAGE"]
    fn released_store_package_is_trusted() {
        let package = std::env::var("TING_TEST_PLUGIN_STORE_PACKAGE")
            .expect("TING_TEST_PLUGIN_STORE_PACKAGE must point to a released .tr package");
        let package = Path::new(&package);
        let contents = read_tr_package(package).unwrap();
        assert_eq!(contents.manifest.plugin_id, "ting-reader-plugin-store");
        assert_eq!(contents.manifest.plugin_version, env!("CARGO_PKG_VERSION"));
        assert!(matches!(
            verify_tr_package_signature(package).unwrap(),
            TrPackageSignatureStatus::Trusted { .. }
        ));
    }

    /// Point TING_TEST_OFFICIAL_PACKAGES at the signed current release directory.
    #[test]
    fn signed_official_packages_are_trusted_and_have_matching_v2_manifests() {
        let Ok(root) = std::env::var("TING_TEST_OFFICIAL_PACKAGES") else {
            return;
        };
        let mut total = 0;
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("tr") {
                continue;
            }
            let archive = read_tr_package(&path).unwrap();
            assert_eq!(archive.manifest.plugin_version, env!("CARGO_PKG_VERSION"));
            let manifest: ting_plugin_contract::manifest::PluginManifest =
                serde_yaml::from_slice(archive.files.get("plugin.yml").unwrap()).unwrap();
            manifest.validate().unwrap();
            assert_eq!(archive.manifest.plugin_id, manifest.id);
            assert!(archive.files.contains_key(&archive.manifest.entry_point));
            assert!(
                matches!(
                    verify_tr_package_signature(&path).unwrap(),
                    TrPackageSignatureStatus::Trusted { .. }
                ),
                "Untrusted official artifact {}",
                path.display()
            );
            total += 1;
        }
        assert_eq!(total, 21);
    }
}

mod archive_limits {
    use super::*;

    #[test]
    fn rejects_too_many_archive_entries() {
        let mut budget = PackageReadBudget {
            entry_count: MAX_PACKAGE_ENTRY_COUNT,
            total_unpacked_bytes: 0,
        };
        assert!(budget.record_entry().is_err());
    }

    #[test]
    fn rejects_oversized_single_file() {
        let mut budget = PackageReadBudget::default();
        assert!(
            budget
                .record_file("plugin.bin", MAX_PACKAGE_SINGLE_FILE_BYTES + 1)
                .is_err()
        );
    }

    #[test]
    fn rejects_oversized_total_unpacked_payload() {
        let mut budget = PackageReadBudget {
            entry_count: 0,
            total_unpacked_bytes: MAX_PACKAGE_TOTAL_UNPACKED_BYTES,
        };
        assert!(budget.record_file("plugin.bin", 1).is_err());
    }
}
