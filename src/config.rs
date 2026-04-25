use anyhow::{bail, Result};
use clap::ValueEnum;
use serde::Serialize;
use std::path::PathBuf;

use crate::cli::Cli;
use crate::repo::AptSource;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, ValueEnum)]
pub enum Arch {
    Amd64,
    Arm64,
}

impl Arch {
    pub fn deb_arch(self) -> &'static str {
        match self {
            Self::Amd64 => "amd64",
            Self::Arm64 => "arm64",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuilderConfig {
    pub target_arches: Vec<Arch>,
    pub ubuntu_codename: String,
    pub mongodb_version: String,
    pub mongo_repo: String,
    pub ubuntu_repo_amd64: String,
    pub ubuntu_repo_arm64: String,
    pub ubuntu_security_repo_amd64: String,
    pub ubuntu_security_repo_arm64: String,
    pub output_amd64: PathBuf,
    pub output_arm64: PathBuf,
    pub include_ca_certs: bool,
    pub roots: Vec<String>,
    pub manifest_dir: PathBuf,
    pub staging_dir: PathBuf,
    pub deb_cache_dir: PathBuf,
}

impl BuilderConfig {
    pub fn from_cli(cli: Cli) -> Result<Self> {
        let mut arches = cli.arches;
        arches.sort();
        arches.dedup();

        if arches.is_empty() {
            bail!("at least one architecture must be specified via --arches");
        }

        Ok(Self {
            target_arches: arches,
            ubuntu_codename: cli.ubuntu_codename,
            mongodb_version: cli.mongodb_version,
            mongo_repo: cli.mongo_repo,
            ubuntu_repo_amd64: cli.ubuntu_repo_amd64,
            ubuntu_repo_arm64: cli.ubuntu_repo_arm64,
            ubuntu_security_repo_amd64: cli.ubuntu_security_repo_amd64,
            ubuntu_security_repo_arm64: cli.ubuntu_security_repo_arm64,
            output_amd64: cli.output_amd64,
            output_arm64: cli.output_arm64,
            include_ca_certs: cli.include_ca_certs,
            roots: cli.roots,
            manifest_dir: cli.manifest_dir,
            staging_dir: cli.staging_dir,
            deb_cache_dir: cli.deb_cache_dir,
        })
    }

    pub fn output_dir_for_arch(&self, arch: Arch) -> &PathBuf {
        match arch {
            Arch::Amd64 => &self.output_amd64,
            Arch::Arm64 => &self.output_arm64,
        }
    }

    pub fn closure_roots(&self) -> Vec<String> {
        let mut roots: Vec<String> = self
            .roots
            .iter()
            .map(|x| x.trim())
            .filter(|x| !x.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        if self.include_ca_certs && !roots.iter().any(|x| x == "ca-certificates") {
            roots.push("ca-certificates".to_string());
        }

        roots.sort();
        roots.dedup();
        roots
    }

    pub fn sources_for_arch(&self, arch: Arch) -> Vec<AptSource> {
        let ubuntu_repo = match arch {
            Arch::Amd64 => self.ubuntu_repo_amd64.clone(),
            Arch::Arm64 => self.ubuntu_repo_arm64.clone(),
        };

        let ubuntu_security_repo = match arch {
            Arch::Amd64 => self.ubuntu_security_repo_amd64.clone(),
            Arch::Arm64 => self.ubuntu_security_repo_arm64.clone(),
        };

        let ubuntu_components = vec![
            "main".to_string(),
            "universe".to_string(),
            "multiverse".to_string(),
        ];

        vec![
            AptSource {
                name: "mongodb".to_string(),
                base_url: self.mongo_repo.clone(),
                suite: format!(
                    "{}/mongodb-org/{}",
                    self.ubuntu_codename, self.mongodb_version
                ),
                components: vec!["multiverse".to_string()],
                arch,
            },
            AptSource {
                name: "ubuntu-security".to_string(),
                base_url: ubuntu_security_repo,
                suite: format!("{}-security", self.ubuntu_codename),
                components: ubuntu_components.clone(),
                arch,
            },
            AptSource {
                name: "ubuntu-updates".to_string(),
                base_url: ubuntu_repo.clone(),
                suite: format!("{}-updates", self.ubuntu_codename),
                components: ubuntu_components.clone(),
                arch,
            },
            AptSource {
                name: "ubuntu".to_string(),
                base_url: ubuntu_repo,
                suite: self.ubuntu_codename.clone(),
                components: ubuntu_components,
                arch,
            },
        ]
    }
}
