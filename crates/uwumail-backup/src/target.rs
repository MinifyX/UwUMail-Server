//! Where the backups go: an SFTP server, an S3 bucket (Amazon or anything that speaks its API,
//! such as MinIO, Backblaze B2, Hetzner Object Storage or Wasabi), or a folder on this machine, for
//! a disk or a network share mounted into it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::sftp::SftpTarget;

/// An S3 bucket, or a part of one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct S3Target {
    /// `https://s3.eu-central-1.amazonaws.com`, `https://fsn1.your-objectstorage.com`, or
    /// `http://minio.lan:9000` for a server in the own network. Without the bucket.
    pub endpoint: String,
    /// `us-east-1`, `eu-central-1`, `fsn1`, …; whatever the provider signs requests for.
    pub region: String,
    pub bucket: String,
    /// The folder inside the bucket, e.g. `uwumail`. Empty for the whole bucket.
    #[serde(default)]
    pub prefix: String,
    pub access_key: String,
    pub secret_key: String,
    /// `https://host/bucket/key` instead of `https://bucket.host/key`. MinIO and many servers in
    /// the own network want this; Amazon and most providers take either.
    #[serde(default)]
    pub path_style: bool,
}

/// A folder on this machine: a second disk, or a NAS share mounted into the container.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderTarget {
    /// An absolute path, e.g. `/backup`.
    pub path: String,
}

/// Where the backups go.
///
/// Settings written before 0.14 hold an SFTP server without saying so; they still read as one.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Target {
    Sftp(SftpTarget),
    S3(S3Target),
    Folder(FolderTarget),
}

/// The tagged form, as written from 0.14 on.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Tagged {
    Sftp(SftpTarget),
    S3(S3Target),
    Folder(FolderTarget),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Stored {
    Tagged(Tagged),
    /// Before 0.14 there was only SFTP, and no tag.
    Sftp(SftpTarget),
}

impl<'de> Deserialize<'de> for Target {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Tagged(Tagged::Sftp(target)) | Stored::Sftp(target) => Target::Sftp(target),
            Stored::Tagged(Tagged::S3(target)) => Target::S3(target),
            Stored::Tagged(Tagged::Folder(target)) => Target::Folder(target),
        })
    }
}

impl Target {
    /// `sftp`, `s3` or `folder`, as the portal and the log call it.
    pub fn kind(&self) -> &'static str {
        match self {
            Target::Sftp(_) => "sftp",
            Target::S3(_) => "s3",
            Target::Folder(_) => "folder",
        }
    }

    /// Where it is, without anything secret: for the log and the audit trail.
    pub fn shown(&self) -> String {
        match self {
            Target::Sftp(sftp) => format!("sftp://{}@{}:{}/{}", sftp.user, sftp.host, sftp.port, sftp.path),
            Target::S3(s3) => {
                let prefix = s3.prefix.trim_matches('/');
                if prefix.is_empty() {
                    format!("s3://{} at {}", s3.bucket, s3.endpoint)
                } else {
                    format!("s3://{}/{prefix} at {}", s3.bucket, s3.endpoint)
                }
            }
            Target::Folder(folder) => folder.path.clone(),
        }
    }

    pub fn as_sftp_mut(&mut self) -> Option<&mut SftpTarget> {
        match self {
            Target::Sftp(sftp) => Some(sftp),
            _ => None,
        }
    }

    pub fn as_sftp(&self) -> Option<&SftpTarget> {
        match self {
            Target::Sftp(sftp) => Some(sftp),
            _ => None,
        }
    }
}

impl FolderTarget {
    pub fn dir(&self) -> PathBuf {
        PathBuf::from(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Login;

    #[test]
    fn settings_from_before_0_14_still_read_as_sftp() {
        let old = r#"{"host":"nas.example.org","port":22,"user":"backup","path":"uwumail",
            "login":{"method":"password","password":"geheim"},"hostKey":"SHA256:abc"}"#;
        let target: Target = serde_json::from_str(old).unwrap();
        let Target::Sftp(sftp) = &target else { panic!("{target:?}") };
        assert_eq!((sftp.host.as_str(), sftp.host_key.as_deref()), ("nas.example.org", Some("SHA256:abc")));
        assert_eq!(sftp.login, Login::Password { password: "geheim".into() });

        // Written again, it carries its kind, and reads back the same.
        let written = serde_json::to_value(&target).unwrap();
        assert_eq!(written["kind"], "sftp");
        assert_eq!(serde_json::from_value::<Target>(written).unwrap(), target);
    }

    #[test]
    fn s3_and_folders_round_trip() {
        let s3 = Target::S3(S3Target {
            endpoint: "https://s3.example.com".into(),
            region: "eu-central-1".into(),
            bucket: "backups".into(),
            prefix: "uwumail".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "geheim".into(),
            path_style: true,
        });
        let json = serde_json::to_string(&s3).unwrap();
        assert!(json.contains(r#""kind":"s3""#), "{json}");
        assert_eq!(serde_json::from_str::<Target>(&json).unwrap(), s3);
        assert_eq!(s3.shown(), "s3://backups/uwumail at https://s3.example.com");

        let folder: Target = serde_json::from_str(r#"{"kind":"folder","path":"/backup"}"#).unwrap();
        assert_eq!(folder, Target::Folder(FolderTarget { path: "/backup".into() }));
        assert!(serde_json::from_str::<Target>(r#"{"kind":"ftp","path":"/x"}"#).is_err());
    }
}
