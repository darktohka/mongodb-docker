mod cli;
mod config;
mod deb;
mod deps;
mod repo;
mod runtime;

use anyhow::{Context, Result};
use clap::Parser;
use rayon::ThreadPoolBuilder;
use reqwest::blocking::Client;
use serde::Serialize;
use std::fs;

use crate::config::BuilderConfig;
use crate::repo::PackageCatalog;

#[derive(Debug, Serialize)]
struct ClosureManifest {
    arch: String,
    output_dir: String,
    ubuntu_codename: String,
    mongodb_version: String,
    include_ca_certs: bool,
    roots: Vec<String>,
    package_count: usize,
    packages: Vec<ManifestPackage>,
}

#[derive(Debug, Serialize)]
struct ManifestPackage {
    name: String,
    version: String,
    source: String,
    source_base_url: String,
    filename: String,
    sha256: String,
    size: u64,
    depends: Option<String>,
    pre_depends: Option<String>,
}

impl From<repo::PackageRecord> for ManifestPackage {
    fn from(value: repo::PackageRecord) -> Self {
        Self {
            name: value.name,
            version: value.version,
            source: value.source,
            source_base_url: value.source_base_url,
            filename: value.filename,
            sha256: value.sha256,
            size: value.size,
            depends: value.depends,
            pre_depends: value.pre_depends,
        }
    }
}

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let config = BuilderConfig::from_cli(cli)?;
    run(config)
}

fn run(config: BuilderConfig) -> Result<()> {
    let client = Client::builder()
        .user_agent("mongodb-appdir-builder/0.1")
        .build()
        .context("failed to build HTTP client")?;

    let download_pool = ThreadPoolBuilder::new()
        .num_threads(config.download_jobs)
        .build()
        .context("failed to build download worker pool")?;

    fs::create_dir_all(&config.manifest_dir)
        .with_context(|| format!("failed to create {}", config.manifest_dir.display()))?;
    fs::create_dir_all(&config.staging_dir)
        .with_context(|| format!("failed to create {}", config.staging_dir.display()))?;
    fs::create_dir_all(&config.deb_cache_dir)
        .with_context(|| format!("failed to create {}", config.deb_cache_dir.display()))?;

    let roots = config.closure_roots();

    for arch in &config.target_arches {
        eprintln!("Resolving package closure for {}", arch.deb_arch());
        let mut catalog = PackageCatalog::default();

        for source in config.sources_for_arch(*arch) {
            let index =
                repo::fetch_source_index(&client, &source, &download_pool).with_context(|| {
                    format!(
                        "failed to fetch package metadata for source {} (arch {})",
                        source.name,
                        arch.deb_arch()
                    )
                })?;
            catalog.ingest(index);
        }

        let closure = deps::resolve_closure(&catalog, &roots).with_context(|| {
            format!("failed to resolve package closure for {}", arch.deb_arch())
        })?;

        let stage_summary = deb::stage_packages_for_arch(
            &client,
            *arch,
            &closure,
            &config.staging_dir,
            &config.deb_cache_dir,
            &download_pool,
        )
        .with_context(|| format!("failed to stage packages for {}", arch.deb_arch()))?;

        let runtime_summary = runtime::emit_minimal_appdir(
            *arch,
            &stage_summary.rootfs_dir,
            config.output_dir_for_arch(*arch),
            &config.manifest_dir,
            config.include_ca_certs,
        )
        .with_context(|| format!("failed to emit runtime appdir for {}", arch.deb_arch()))?;

        let packages: Vec<ManifestPackage> =
            closure.iter().cloned().map(ManifestPackage::from).collect();
        let manifest = ClosureManifest {
            arch: arch.deb_arch().to_string(),
            output_dir: config.output_dir_for_arch(*arch).display().to_string(),
            ubuntu_codename: config.ubuntu_codename.clone(),
            mongodb_version: config.mongodb_version.clone(),
            include_ca_certs: config.include_ca_certs,
            roots: roots.clone(),
            package_count: packages.len(),
            packages,
        };

        let manifest_path = config
            .manifest_dir
            .join(format!("{}-closure.json", arch.deb_arch()));
        let body =
            serde_json::to_string_pretty(&manifest).context("failed to serialize manifest")?;
        fs::write(&manifest_path, format!("{}\n", body))
            .with_context(|| format!("failed to write {}", manifest_path.display()))?;

        println!(
            "wrote {} ({} packages)",
            manifest_path.display(),
            manifest.package_count
        );
        println!(
            "staged {} packages into {} (downloaded {} packages, {} bytes)",
            stage_summary.package_count,
            stage_summary.rootfs_dir.display(),
            stage_summary.downloaded_count,
            stage_summary.downloaded_bytes
        );
        println!(
            "emitted minimal appdir at {} ({} files)",
            runtime_summary.output_dir.display(),
            runtime_summary.copied_file_count
        );
        println!("wrote {}", runtime_summary.runtime_manifest_path.display());
    }

    Ok(())
}
