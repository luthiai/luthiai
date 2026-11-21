//! Verify driver-specific reviewer images and retain trusted image environments.

use crate::archive::{self, ArchiveError};
use crate::config::{Driver, ReviewImage};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Evidence that local image inputs matched their configured checksums.
#[must_use = "pass this evidence to model validation"]
pub struct VerifiedImage<'a> {
    image: &'a ReviewImage,
    environment: BTreeMap<String, String>,
}

impl VerifiedImage<'_> {
    pub fn matches(&self, image: &ReviewImage) -> bool {
        self.image == image
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }
}

/// A private verified VM archive or pinned OCI reference, ready for creation.
pub struct StagedImage {
    driver: Driver,
    source: OsString,
    environment: BTreeMap<String, String>,
}

impl StagedImage {
    pub fn driver(&self) -> Driver {
        self.driver
    }

    pub fn source(&self) -> &OsStr {
        &self.source
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }
}

pub fn verify_image(image: &ReviewImage) -> Result<VerifiedImage<'_>, ArchiveError> {
    let environment = match image {
        ReviewImage::VmArchive(archive) => archive::verify_archive(archive)?.environment().clone(),
        ReviewImage::OciImage(image) => {
            let (_, bytes) = archive::verify_file(
                image.config_path(),
                image.config_checksum(),
                Some(1024 * 1024),
            )?;
            archive::parse_environment(&bytes)?
        }
    };
    Ok(VerifiedImage { image, environment })
}

pub fn stage_image(image: &ReviewImage, directory: &Path) -> Result<StagedImage, ArchiveError> {
    match image {
        ReviewImage::VmArchive(archive) => {
            let staged = archive::stage_archive(archive, directory)?;
            Ok(StagedImage {
                driver: Driver::Vm,
                source: staged.path().as_os_str().to_owned(),
                environment: staged.environment().clone(),
            })
        }
        ReviewImage::OciImage(oci) => {
            let verified = verify_image(image)?;
            Ok(StagedImage {
                driver: Driver::Kubernetes,
                source: oci.reference().into(),
                environment: verified.environment,
            })
        }
    }
}

#[cfg(test)]
impl<'a> VerifiedImage<'a> {
    pub(crate) fn for_test(image: &'a ReviewImage) -> Self {
        Self {
            image,
            environment: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
impl StagedImage {
    pub(crate) fn for_test(
        path: std::path::PathBuf,
        environment: BTreeMap<String, String>,
    ) -> Self {
        Self {
            driver: Driver::Vm,
            source: path.into_os_string(),
            environment,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;

    #[test]
    fn oci_metadata_is_verified_and_frozen_without_a_local_archive() {
        let directory = tempfile::tempdir().unwrap();
        let metadata = directory.path().join("image.config.json");
        let bytes = br#"{"config":{"Env":["CLAUDE_CODE_EXECUTABLE=/usr/bin/claude"]}}"#;
        fs::write(&metadata, bytes).unwrap();
        let mut raw = serde_json::json!({"oci_image": {
            "reference": format!("registry.example.com/reviewer@sha256:{}", "a".repeat(64)),
            "config_path": metadata,
            "config_sha256": format!("{:x}", Sha256::digest(bytes)),
        }});
        let image: ReviewImage = serde_json::from_value(raw.clone()).unwrap();
        let verified = verify_image(&image).unwrap();
        assert!(verified.matches(&image));
        let staged = stage_image(&image, &directory.path().join("unused-archives")).unwrap();
        assert_eq!(staged.driver(), Driver::Kubernetes);
        assert_eq!(
            staged.source(),
            raw["oci_image"]["reference"].as_str().unwrap()
        );
        assert!(!directory.path().join("unused-archives").exists());
        fs::write(&metadata, b"changed metadata").unwrap();
        assert_eq!(
            staged.environment()["CLAUDE_CODE_EXECUTABLE"],
            "/usr/bin/claude"
        );
        assert!(matches!(
            verify_image(&image),
            Err(ArchiveError::ChecksumMismatch { .. })
        ));
        raw["oci_image"]["reference"] =
            format!("registry.example.com/reviewer@sha256:{}", "b".repeat(64)).into();
        let changed: ReviewImage = serde_json::from_value(raw).unwrap();
        assert!(!verified.matches(&changed));
        #[cfg(unix)]
        {
            fs::remove_file(&metadata).unwrap();
            let target = directory.path().join("target.json");
            fs::write(&target, bytes).unwrap();
            std::os::unix::fs::symlink(target, &metadata).unwrap();
            assert!(matches!(
                verify_image(&image),
                Err(ArchiveError::NotRegular(_))
            ));
        }
    }
}
