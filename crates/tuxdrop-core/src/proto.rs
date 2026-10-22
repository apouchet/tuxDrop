//! LocalSend protocol v2 wire types.
//!
//! Spec: <https://github.com/localsend/protocol>

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "2.1";
pub const DEFAULT_PORT: u16 = 53317;
pub const MULTICAST_ADDR: std::net::Ipv4Addr = std::net::Ipv4Addr::new(224, 0, 0, 167);
pub const API_PREFIX: &str = "/api/localsend/v2";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Mobile,
    Desktop,
    Web,
    Headless,
    Server,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Https,
}

impl Protocol {
    pub fn scheme(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Https => "https",
        }
    }

    pub fn from_scheme(scheme: &str) -> Option<Self> {
        match scheme {
            "http" => Some(Protocol::Http),
            "https" => Some(Protocol::Https),
            _ => None,
        }
    }
}

/// Device description shared by announcements, `register` and `prepare-upload`.
///
/// `port` and `protocol` are absent from `register` responses.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub alias: String,
    pub version: String,
    #[serde(default)]
    pub device_model: Option<String>,
    #[serde(default)]
    pub device_type: Option<DeviceType>,
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Protocol>,
    #[serde(default)]
    pub download: bool,
}

/// UDP multicast message: device info plus the `announce` flag.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Announcement {
    #[serde(flatten)]
    pub info: DeviceInfo,
    #[serde(default)]
    pub announce: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct FileMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessed: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    pub id: String,
    /// May contain `/`-separated relative paths when a folder is sent.
    pub file_name: String,
    pub size: u64,
    pub file_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<FileMetadata>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrepareUploadRequest {
    pub info: DeviceInfo,
    pub files: HashMap<String, FileMeta>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadResponse {
    pub session_id: String,
    /// fileId -> token
    pub files: HashMap<String, String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UploadQuery {
    pub session_id: String,
    pub file_id: String,
    pub token: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionQuery {
    pub session_id: String,
}

#[derive(Deserialize, Debug, Default)]
pub struct PinQuery {
    pub pin: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_announcement() {
        let json = r#"{
            "alias": "Nice Orange",
            "version": "2.0",
            "deviceModel": "Samsung",
            "deviceType": "mobile",
            "fingerprint": "random string",
            "port": 53317,
            "protocol": "https",
            "download": true,
            "announce": true
        }"#;
        let a: Announcement = serde_json::from_str(json).unwrap();
        assert!(a.announce);
        assert_eq!(a.info.alias, "Nice Orange");
        assert_eq!(a.info.device_type, Some(DeviceType::Mobile));
        assert_eq!(a.info.port, Some(53317));
        assert_eq!(a.info.protocol, Some(Protocol::Https));
    }

    #[test]
    fn parses_register_response_without_port() {
        let json = r#"{"alias":"Secret Banana","version":"2.0","deviceModel":null,
            "deviceType":"desktop","fingerprint":"abc","download":false}"#;
        let i: DeviceInfo = serde_json::from_str(json).unwrap();
        assert_eq!(i.port, None);
        assert_eq!(i.device_model, None);
    }

    #[test]
    fn unknown_device_type_does_not_fail() {
        let json = r#"{"alias":"x","version":"2.0","deviceType":"toaster","fingerprint":"f"}"#;
        let i: DeviceInfo = serde_json::from_str(json).unwrap();
        assert_eq!(i.device_type, Some(DeviceType::Unknown));
    }

    #[test]
    fn parses_spec_prepare_upload() {
        let json = r#"{
            "info": {"alias":"Nice Orange","version":"2.0","deviceModel":"Samsung",
                     "deviceType":"mobile","fingerprint":"random string","port":53317,
                     "protocol":"https","download":true},
            "files": {
                "some file id": {"id":"some file id","fileName":"my image.png","size":324242,
                    "fileType":"image/jpeg","sha256":"*sha256 hash*","preview":"*preview data*",
                    "metadata":{"modified":"2021-01-01T12:34:56Z","accessed":"2021-01-01T12:34:56Z"}},
                "another file id": {"id":"another file id","fileName":"another image.jpg",
                    "size":1234,"fileType":"image/jpeg"}
            }
        }"#;
        let r: PrepareUploadRequest = serde_json::from_str(json).unwrap();
        assert_eq!(r.files.len(), 2);
        assert_eq!(r.files["another file id"].sha256, None);
        assert_eq!(
            r.files["some file id"]
                .metadata
                .as_ref()
                .unwrap()
                .modified
                .as_deref(),
            Some("2021-01-01T12:34:56Z")
        );
    }

    #[test]
    fn serializes_camel_case() {
        let r = PrepareUploadResponse {
            session_id: "s".into(),
            files: HashMap::from([("f".into(), "t".into())]),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["sessionId"], "s");
        assert_eq!(v["files"]["f"], "t");
    }
}
