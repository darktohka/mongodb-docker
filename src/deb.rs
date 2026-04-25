use anyhow::{anyhow, bail, Context, Result};
use ar::Archive as ArArchive;
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use rayon::prelude::*;
use rayon::ThreadPool;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use tar::Archive as TarArchive;
use xz2::read::XzDecoder;

use crate::config::Arch;
use crate::repo::PackageRecord;

#[derive(Debug)]
pub struct StageSummary {
    pub rootfs_dir: PathBuf,
    pub package_count: usize,
    pub downloaded_count: usize,
    pub downloaded_bytes: u64,
}

pub fn stage_packages_for_arch(
    client: &Client,
    arch: Arch,
    packages: &[PackageRecord],
    staging_root: &Path,
    deb_cache_root: &Path,
    download_pool: &ThreadPool,
) -> Result<StageSummary> {
    let rootfs_dir = staging_root.join(arch.deb_arch());
    let cache_dir = deb_cache_root.join(arch.deb_arch());

    prepare_clean_dir(&rootfs_dir)
        .with_context(|| format!("failed to prepare {}", rootfs_dir.display()))?;
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("failed to create {}", cache_dir.display()))?;

    let cached_deb_paths: Vec<PathBuf> = packages
        .iter()
        .map(|package| cache_path_for_package(&cache_dir, package))
        .collect();

    let download_results = download_pool.install(|| {
        packages
            .par_iter()
            .zip(cached_deb_paths.par_iter())
            .map(|(package, cached_deb_path)| {
                ensure_deb_cached(client, package, cached_deb_path)
                    .with_context(|| format!("failed to cache {}", package.name))
            })
            .collect::<Vec<Result<bool>>>()
    });

    let mut downloaded_count = 0usize;
    let mut downloaded_bytes = 0u64;

    for (downloaded_result, package) in download_results.into_iter().zip(packages.iter()) {
        let downloaded = downloaded_result?;
        if downloaded {
            downloaded_count += 1;
            downloaded_bytes += package.size;
        }
    }

    for (package, cached_deb_path) in packages.iter().zip(cached_deb_paths.iter()) {
        extract_data_archive_from_deb(&cached_deb_path, &rootfs_dir).with_context(|| {
            format!(
                "failed to extract package {} ({})",
                package.name,
                cached_deb_path.display()
            )
        })?;
    }

    Ok(StageSummary {
        rootfs_dir,
        package_count: packages.len(),
        downloaded_count,
        downloaded_bytes,
    })
}

fn prepare_clean_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove existing {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    Ok(())
}

fn ensure_deb_cached(client: &Client, package: &PackageRecord, path: &Path) -> Result<bool> {
    if path.exists() {
        let blob = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        verify_package_blob(&blob, package, &path.display().to_string())?;
        return Ok(false);
    }

    let url = package_download_url(package);
    let blob = fetch_bytes(client, &url)
        .with_context(|| format!("failed to download deb payload from {}", url))?;
    verify_package_blob(&blob, package, &url)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    fs::write(path, blob).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

fn package_download_url(package: &PackageRecord) -> String {
    format!(
        "{}/{}",
        package.source_base_url.trim_end_matches('/'),
        package.filename.trim_start_matches('/')
    )
}

fn cache_path_for_package(cache_dir: &Path, package: &PackageRecord) -> PathBuf {
    let short_hash = &package.sha256[..16.min(package.sha256.len())];
    let safe_name = sanitize_file_component(&package.name);
    cache_dir.join(format!("{}-{}.deb", safe_name, short_hash))
}

fn sanitize_file_component(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn fetch_bytes(client: &Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("request failed for {}", url))?
        .error_for_status()
        .with_context(|| format!("server returned error status for {}", url))?;

    let bytes = response
        .bytes()
        .with_context(|| format!("failed to read response body for {}", url))?;
    Ok(bytes.to_vec())
}

fn verify_package_blob(blob: &[u8], package: &PackageRecord, source: &str) -> Result<()> {
    let actual_size = blob.len() as u64;
    if actual_size != package.size {
        bail!(
            "size mismatch for {} ({}): expected {}, got {}",
            package.name,
            source,
            package.size,
            actual_size
        );
    }

    let actual_hash = format!("{:x}", Sha256::digest(blob));
    if actual_hash != package.sha256 {
        bail!(
            "sha256 mismatch for {} ({}): expected {}, got {}",
            package.name,
            source,
            package.sha256,
            actual_hash
        );
    }

    Ok(())
}

fn extract_data_archive_from_deb(deb_path: &Path, destination: &Path) -> Result<()> {
    let file = File::open(deb_path)
        .with_context(|| format!("failed to open deb archive {}", deb_path.display()))?;
    let mut archive = ArArchive::new(file);

    while let Some(entry_result) = archive.next_entry() {
        let mut entry = entry_result
            .with_context(|| format!("failed to read ar entry from {}", deb_path.display()))?;
        let member_name = normalize_ar_identifier(entry.header().identifier())
            .context("ar member name is not valid UTF-8")?;

        if !member_name.starts_with("data.tar") {
            continue;
        }

        let mut payload = Vec::new();
        entry.read_to_end(&mut payload).with_context(|| {
            format!("failed to read {} from {}", member_name, deb_path.display())
        })?;

        return extract_tar_payload(&member_name, &payload, destination);
    }

    Err(anyhow!(
        "missing data.tar member in deb archive {}",
        deb_path.display()
    ))
}

fn normalize_ar_identifier(identifier: &[u8]) -> Result<String> {
    let raw = std::str::from_utf8(identifier)?;
    Ok(raw.trim().trim_end_matches('/').to_string())
}

fn extract_tar_payload(member_name: &str, payload: &[u8], destination: &Path) -> Result<()> {
    if member_name.ends_with(".tar") {
        let mut archive = TarArchive::new(Cursor::new(payload));
        archive
            .unpack(destination)
            .with_context(|| format!("failed to unpack {}", member_name))?;
        return Ok(());
    }

    if member_name.ends_with(".tar.gz") {
        let decoder = GzDecoder::new(Cursor::new(payload));
        let mut archive = TarArchive::new(decoder);
        archive
            .unpack(destination)
            .with_context(|| format!("failed to unpack {}", member_name))?;
        return Ok(());
    }

    if member_name.ends_with(".tar.xz") {
        let decoder = XzDecoder::new(Cursor::new(payload));
        let mut archive = TarArchive::new(decoder);
        archive
            .unpack(destination)
            .with_context(|| format!("failed to unpack {}", member_name))?;
        return Ok(());
    }

    if member_name.ends_with(".tar.bz2") {
        let decoder = BzDecoder::new(Cursor::new(payload));
        let mut archive = TarArchive::new(decoder);
        archive
            .unpack(destination)
            .with_context(|| format!("failed to unpack {}", member_name))?;
        return Ok(());
    }

    if member_name.ends_with(".tar.zst") {
        let decoder = zstd::stream::read::Decoder::new(Cursor::new(payload))
            .with_context(|| format!("failed to initialize zstd for {}", member_name))?;
        let mut archive = TarArchive::new(decoder);
        archive
            .unpack(destination)
            .with_context(|| format!("failed to unpack {}", member_name))?;
        return Ok(());
    }

    bail!(
        "unsupported data member compression format: {}",
        member_name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_package() -> PackageRecord {
        PackageRecord {
            name: "mongodb-org-server".to_string(),
            version: "8.2.0".to_string(),
            source: "mongodb".to_string(),
            source_base_url: "https://repo.mongodb.org/apt/ubuntu".to_string(),
            filename: "pool/multiverse/m/mongodb-org/mongodb-org-server_8.2.0_amd64.deb"
                .to_string(),
            sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_string(),
            size: 5,
            depends: None,
            pre_depends: None,
        }
    }

    #[test]
    fn package_url_joins_base_and_filename() {
        let package = sample_package();
        let url = package_download_url(&package);
        assert_eq!(
            url,
            "https://repo.mongodb.org/apt/ubuntu/pool/multiverse/m/mongodb-org/mongodb-org-server_8.2.0_amd64.deb"
        );
    }

    #[test]
    fn cache_path_sanitizes_package_name() {
        let package = PackageRecord {
            name: "libgcc-s1:any".to_string(),
            ..sample_package()
        };
        let cache_path = cache_path_for_package(Path::new("/tmp/cache"), &package);
        assert!(cache_path
            .file_name()
            .expect("filename")
            .to_string_lossy()
            .starts_with("libgcc-s1_any-"));
    }

    #[test]
    fn blob_verification_checks_size_and_hash() {
        let package = sample_package();
        verify_package_blob(b"hello", &package, "unit-test").expect("verification should pass");
    }
}
