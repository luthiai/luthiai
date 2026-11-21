//! Verify driver-specific reviewer images and retain trusted image environments.

use crate::archive::{self, ArchiveError};
use crate::config::{Driver, ReviewImage};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// A locally verified VM archive or a validated, digest-pinned OCI image identity.
#[must_use = "pass this evidence to model validation"]
pub struct VerifiedImage<'a> {
    image: &'a ReviewImage,
}

impl VerifiedImage<'_> {
    pub fn matches(&self, image: &ReviewImage) -> bool {
        self.image == image
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
    if let ReviewImage::VmArchive(archive) = image {
        let _verified = archive::verify_archive(archive)?;
    }
    Ok(VerifiedImage { image })
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
        ReviewImage::OciImage(oci) => Ok(StagedImage {
            driver: Driver::Kubernetes,
            source: oci.reference().into(),
            environment: BTreeMap::new(),
        }),
    }
}

#[cfg(test)]
impl<'a> VerifiedImage<'a> {
    pub(crate) fn for_test(image: &'a ReviewImage) -> Self {
        Self { image }
    }
}

#[cfg(all(test, unix))]
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

    #[test]
    fn pinned_oci_image_needs_no_local_files_or_environment() {
        let directory = tempfile::tempdir().unwrap();
        let mut raw = serde_json::json!({"oci_image": format!("registry.example.com/reviewer@sha256:{}", "a".repeat(64))});
        let image: ReviewImage = serde_json::from_value(raw.clone()).unwrap();
        let verified = verify_image(&image).unwrap();
        assert!(verified.matches(&image));
        let staged = stage_image(&image, &directory.path().join("unused-archives")).unwrap();
        assert_eq!(staged.driver(), Driver::Kubernetes);
        assert_eq!(staged.source(), raw["oci_image"].as_str().unwrap());
        assert!(!directory.path().join("unused-archives").exists());
        assert!(staged.environment().is_empty());
        raw["oci_image"] =
            format!("registry.example.com/reviewer@sha256:{}", "b".repeat(64)).into();
        let changed: ReviewImage = serde_json::from_value(raw).unwrap();
        assert!(!verified.matches(&changed));
    }
}
