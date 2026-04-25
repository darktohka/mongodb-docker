use clap::{ArgAction, Parser};
use std::path::PathBuf;

use crate::config::Arch;

#[derive(Debug, Clone, Parser)]
#[command(
    author,
    version,
    about = "Build MongoDB AppDir metadata for amd64 and arm64"
)]
pub struct Cli {
    #[arg(long, value_delimiter = ',', default_value = "amd64,arm64")]
    pub arches: Vec<Arch>,

    #[arg(long, default_value = "jammy")]
    pub ubuntu_codename: String,

    #[arg(long, default_value = "8.2")]
    pub mongodb_version: String,

    #[arg(long, default_value = "https://repo.mongodb.org/apt/ubuntu")]
    pub mongo_repo: String,

    #[arg(long, default_value = "http://archive.ubuntu.com/ubuntu")]
    pub ubuntu_repo_amd64: String,

    #[arg(long, default_value = "http://ports.ubuntu.com/ubuntu-ports")]
    pub ubuntu_repo_arm64: String,

    #[arg(long, default_value = "http://security.ubuntu.com/ubuntu")]
    pub ubuntu_security_repo_amd64: String,

    #[arg(long, default_value = "http://ports.ubuntu.com/ubuntu-ports")]
    pub ubuntu_security_repo_arm64: String,

    #[arg(long, default_value = "binary-x86_64")]
    pub output_amd64: PathBuf,

    #[arg(long, default_value = "binary-aarch64")]
    pub output_arm64: PathBuf,

    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub include_ca_certs: bool,

    #[arg(long, value_delimiter = ',', default_value = "mongodb-org-server")]
    pub roots: Vec<String>,

    #[arg(long, default_value = "target/package-manifests")]
    pub manifest_dir: PathBuf,

    #[arg(long, default_value = "target/staging-rootfs")]
    pub staging_dir: PathBuf,

    #[arg(long, default_value = "target/deb-cache")]
    pub deb_cache_dir: PathBuf,
}
