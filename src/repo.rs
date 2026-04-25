use anyhow::{anyhow, bail, Context, Result};
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use rayon::prelude::*;
use rayon::ThreadPool;
use reqwest::blocking::Client;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use xz2::read::XzDecoder;

use crate::config::Arch;

#[derive(Debug, Clone)]
pub struct AptSource {
    pub name: String,
    pub base_url: String,
    pub suite: String,
    pub components: Vec<String>,
    pub arch: Arch,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageRecord {
    pub name: String,
    pub version: String,
    pub source: String,
    pub source_base_url: String,
    pub filename: String,
    pub sha256: String,
    pub size: u64,
    pub depends: Option<String>,
    pub pre_depends: Option<String>,
}

#[derive(Debug, Default)]
pub struct PackageCatalog {
    packages: BTreeMap<String, PackageRecord>,
}

impl PackageCatalog {
    pub fn ingest(&mut self, mut index: PackageIndex) {
        index.packages.sort_by(|a, b| a.name.cmp(&b.name));
        for package in index.packages {
            self.packages.entry(package.name.clone()).or_insert(package);
        }
    }

    pub fn get(&self, package: &str) -> Option<&PackageRecord> {
        self.packages.get(package)
    }

    pub fn contains(&self, package: &str) -> bool {
        self.packages.contains_key(package)
    }
}

#[derive(Debug)]
pub struct PackageIndex {
    pub packages: Vec<PackageRecord>,
}

#[derive(Debug, Clone)]
struct ReleaseChecksum {
    sha256: String,
    size: u64,
}

pub fn fetch_source_index(
    client: &Client,
    source: &AptSource,
    download_pool: &ThreadPool,
) -> Result<PackageIndex> {
    let release_text = fetch_release_text(client, source)?;
    let checksums = parse_release_sha256(&release_text).with_context(|| {
        format!(
            "failed to parse SHA256 section from release metadata for {}",
            source.name
        )
    })?;

    let component_results = download_pool.install(|| {
        source
            .components
            .par_iter()
            .map(|component| fetch_component_packages(client, source, &checksums, component))
            .collect::<Vec<Result<Vec<PackageRecord>>>>()
    });

    let mut packages = Vec::new();
    for result in component_results {
        let mut component_packages = result?;
        packages.append(&mut component_packages);
    }

    if packages.is_empty() {
        bail!(
            "no Packages index found for source {} (suite {}, arch {})",
            source.name,
            source.suite,
            source.arch.deb_arch()
        );
    }

    Ok(PackageIndex { packages })
}

fn fetch_component_packages(
    client: &Client,
    source: &AptSource,
    checksums: &BTreeMap<String, ReleaseChecksum>,
    component: &str,
) -> Result<Vec<PackageRecord>> {
    let candidates = select_packages_index_paths(component, source.arch, checksums);
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut last_error: Option<anyhow::Error> = None;
    for relative_path in candidates {
        match fetch_and_parse_index(client, source, &relative_path, checksums) {
            Ok(parsed) => return Ok(parsed),
            Err(err) => {
                last_error = Some(err);
            }
        }
    }

    let err = last_error.unwrap_or_else(|| anyhow!("no package index candidates were usable"));
    Err(err).with_context(|| {
        format!(
            "all package index candidates failed for component {} from source {}",
            component, source.name
        )
    })
}

fn fetch_and_parse_index(
    client: &Client,
    source: &AptSource,
    relative_path: &str,
    checksums: &BTreeMap<String, ReleaseChecksum>,
) -> Result<Vec<PackageRecord>> {
    let checksum = checksums
        .get(relative_path)
        .ok_or_else(|| anyhow!("missing checksum for {}", relative_path))?;
    let url = format!(
        "{}/dists/{}/{}",
        source.base_url.trim_end_matches('/'),
        source.suite,
        relative_path
    );

    let payload = fetch_bytes(client, &url)
        .with_context(|| format!("failed to download package index from {}", url))?;
    verify_blob(&payload, checksum, &url)?;

    let decompressed = decompress_index(relative_path, &payload)
        .with_context(|| format!("failed to decompress {}", relative_path))?;

    parse_packages(&decompressed, &source.name, &source.base_url)
        .with_context(|| format!("failed to parse {}", relative_path))
}

fn fetch_release_text(client: &Client, source: &AptSource) -> Result<String> {
    let base = source.base_url.trim_end_matches('/');
    let inrelease_url = format!("{}/dists/{}/InRelease", base, source.suite);
    let release_url = format!("{}/dists/{}/Release", base, source.suite);

    match fetch_text(client, &inrelease_url) {
        Ok(body) => Ok(body),
        Err(_) => fetch_text(client, &release_url)
            .with_context(|| format!("failed to download {}", release_url)),
    }
}

fn fetch_text(client: &Client, url: &str) -> Result<String> {
    let body = fetch_bytes(client, url)?;
    String::from_utf8(body).with_context(|| format!("{} is not valid UTF-8", url))
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

fn verify_blob(blob: &[u8], checksum: &ReleaseChecksum, url: &str) -> Result<()> {
    let actual_size = blob.len() as u64;
    if checksum.size != actual_size {
        bail!(
            "size mismatch for {}: expected {}, got {}",
            url,
            checksum.size,
            actual_size
        );
    }

    let actual_hash = format!("{:x}", Sha256::digest(blob));
    if actual_hash != checksum.sha256 {
        bail!(
            "sha256 mismatch for {}: expected {}, got {}",
            url,
            checksum.sha256,
            actual_hash
        );
    }

    Ok(())
}

fn select_packages_index_paths(
    component: &str,
    arch: Arch,
    checksums: &BTreeMap<String, ReleaseChecksum>,
) -> Vec<String> {
    const PREFERRED_SUFFIXES: [&str; 5] = [
        "Packages.xz",
        "Packages.gz",
        "Packages.bz2",
        "Packages.zst",
        "Packages",
    ];

    let mut paths = Vec::new();
    for suffix in PREFERRED_SUFFIXES {
        let candidate = format!("{}/binary-{}/{}", component, arch.deb_arch(), suffix);
        if checksums.contains_key(&candidate) {
            paths.push(candidate);
        }
    }

    paths
}

fn decompress_index(relative_path: &str, compressed: &[u8]) -> Result<String> {
    let data = if relative_path.ends_with(".xz") {
        let mut decoder = XzDecoder::new(Cursor::new(compressed));
        let mut buf = Vec::new();
        decoder.read_to_end(&mut buf)?;
        buf
    } else if relative_path.ends_with(".gz") {
        let mut decoder = GzDecoder::new(Cursor::new(compressed));
        let mut buf = Vec::new();
        decoder.read_to_end(&mut buf)?;
        buf
    } else if relative_path.ends_with(".bz2") {
        let mut decoder = BzDecoder::new(Cursor::new(compressed));
        let mut buf = Vec::new();
        decoder.read_to_end(&mut buf)?;
        buf
    } else if relative_path.ends_with(".zst") {
        zstd::stream::decode_all(Cursor::new(compressed))?
    } else {
        compressed.to_vec()
    };

    String::from_utf8(data).context("decompressed Packages index is not valid UTF-8")
}

fn parse_release_sha256(body: &str) -> Result<BTreeMap<String, ReleaseChecksum>> {
    let mut in_sha256 = false;
    let mut checksums = BTreeMap::new();

    for line in body.lines() {
        if line.trim() == "SHA256:" {
            in_sha256 = true;
            continue;
        }

        if !in_sha256 {
            continue;
        }

        if !line.starts_with(' ') && line.contains(':') {
            break;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            continue;
        }

        let size: u64 = parts[1]
            .parse()
            .with_context(|| format!("invalid size in SHA256 entry: {}", line))?;

        checksums.insert(
            parts[2].to_string(),
            ReleaseChecksum {
                sha256: parts[0].to_ascii_lowercase(),
                size,
            },
        );
    }

    if checksums.is_empty() {
        bail!("Release metadata does not contain a parsable SHA256 section");
    }

    Ok(checksums)
}

fn parse_packages(
    index_text: &str,
    source_name: &str,
    source_base_url: &str,
) -> Result<Vec<PackageRecord>> {
    let mut records = Vec::new();
    for stanza in parse_control_stanzas(index_text) {
        let name = match stanza.get("Package") {
            Some(v) => v.to_string(),
            None => continue,
        };

        let version = match stanza.get("Version") {
            Some(v) => v.to_string(),
            None => continue,
        };

        let filename = match stanza.get("Filename") {
            Some(v) => v.to_string(),
            None => continue,
        };

        let sha256 = match stanza.get("SHA256") {
            Some(v) => v.to_ascii_lowercase(),
            None => continue,
        };

        let size: u64 = match stanza.get("Size") {
            Some(v) => v
                .parse()
                .with_context(|| format!("invalid Size for package {}", name))?,
            None => continue,
        };

        records.push(PackageRecord {
            name,
            version,
            source: source_name.to_string(),
            source_base_url: source_base_url.to_string(),
            filename,
            sha256,
            size,
            depends: stanza.get("Depends").cloned(),
            pre_depends: stanza.get("Pre-Depends").cloned(),
        });
    }

    Ok(records)
}

fn parse_control_stanzas(body: &str) -> Vec<BTreeMap<String, String>> {
    let mut stanzas = Vec::new();
    let mut current: BTreeMap<String, String> = BTreeMap::new();
    let mut last_key: Option<String> = None;

    for line in body.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                stanzas.push(current);
                current = BTreeMap::new();
                last_key = None;
            }
            continue;
        }

        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(key) = &last_key {
                if let Some(value) = current.get_mut(key) {
                    value.push('\n');
                    value.push_str(line.trim());
                }
            }
            continue;
        }

        if let Some((raw_key, raw_value)) = line.split_once(':') {
            let key = raw_key.trim().to_string();
            let value = raw_value.trim().to_string();
            current.insert(key.clone(), value);
            last_key = Some(key);
        }
    }

    if !current.is_empty() {
        stanzas.push(current);
    }

    stanzas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_sha_section() {
        let release = r#"Origin: Ubuntu
Label: Ubuntu
SHA256:
 abcdef 42 main/binary-amd64/Packages.gz
 012345 99 main/binary-amd64/Packages.xz
MD5Sum:
 aa 1 test
"#;

        let parsed = parse_release_sha256(release).expect("release parse should succeed");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed["main/binary-amd64/Packages.gz"].size, 42);
    }

    #[test]
    fn parses_control_stanzas_multiline_fields() {
        let text = r#"Package: testpkg
Version: 1.0
Depends: libc6 (>= 2.34),
 libgcc-s1
Filename: pool/main/t/testpkg.deb
SHA256: deadbeef
Size: 10

"#;

        let records = parse_packages(text, "ubuntu", "http://archive.ubuntu.com/ubuntu")
            .expect("packages parse should succeed");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "testpkg");
        assert_eq!(
            records[0].source_base_url,
            "http://archive.ubuntu.com/ubuntu"
        );
        assert!(records[0]
            .depends
            .as_ref()
            .expect("depends should exist")
            .contains("libgcc-s1"));
    }
}
