//! Immutable linked-resource descriptor; decoded buffers and filesystem access stay in hosts.
use crate::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageAsset {
    path: String,
    sha256: [u8; 32],
    width: u32,
    height: u32,
}
impl ImageAsset {
    pub fn new(path: String, sha256: [u8; 32], width: u32, height: u32) -> Result<Self, Error> {
        let asset = Self {
            path,
            sha256,
            width,
            height,
        };
        asset.validate()?;
        Ok(asset)
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.path.trim().is_empty()
            || self.path.len() > 4096
            || self.path.chars().any(char::is_control)
        {
            return Err(Error::Invalid(
                "image reference requires 1..=4096 UTF-8 path bytes without controls",
            ));
        }
        if self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > 16_777_216
        {
            return Err(Error::Invalid(
                "image dimensions exceed native image resource budget",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_descriptor_validates_metadata_without_os_access() {
        let asset = ImageAsset::new("C:/images/прозорий.png".into(), [7; 32], 2, 3).unwrap();
        assert_eq!(asset.width(), 2);
        assert_eq!(asset.height(), 3);
        assert_eq!(asset.sha256(), &[7; 32]);
        assert!(ImageAsset::new("a\0.png".into(), [0; 32], 1, 1).is_err());
        assert!(ImageAsset::new("a.png".into(), [0; 32], 8192, 8192).is_err());
        assert!(ImageAsset::new("a.png".into(), [0; 32], 1, 0).is_err());
    }
}
