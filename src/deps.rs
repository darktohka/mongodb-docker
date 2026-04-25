use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeSet, VecDeque};

use crate::repo::{PackageCatalog, PackageRecord};

pub fn resolve_closure(catalog: &PackageCatalog, roots: &[String]) -> Result<Vec<PackageRecord>> {
    let mut queue: VecDeque<String> = roots
        .iter()
        .map(|x| normalize_dependency_name(x))
        .filter(|x| !x.is_empty())
        .collect();

    let mut visited = BTreeSet::new();
    let mut resolved = Vec::new();

    while let Some(current) = queue.pop_front() {
        if !visited.insert(current.clone()) {
            continue;
        }

        let package = catalog
            .get(&current)
            .with_context(|| format!("missing package in metadata: {}", current))?
            .clone();

        enqueue_dependencies(catalog, &package, &mut queue)?;
        resolved.push(package);
    }

    resolved.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(resolved)
}

fn enqueue_dependencies(
    catalog: &PackageCatalog,
    package: &PackageRecord,
    queue: &mut VecDeque<String>,
) -> Result<()> {
    for raw_field in [&package.pre_depends, &package.depends] {
        let Some(raw) = raw_field else {
            continue;
        };

        for alternatives in parse_dependency_field(raw) {
            let selected = alternatives
                .iter()
                .find(|candidate| catalog.contains(candidate))
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "unable to satisfy dependency {:?} required by {}",
                        alternatives,
                        package.name
                    )
                })?;
            queue.push_back(selected);
        }
    }

    Ok(())
}

fn parse_dependency_field(raw: &str) -> Vec<Vec<String>> {
    raw.split(',')
        .map(|group| {
            group
                .split('|')
                .map(normalize_dependency_name)
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|group| !group.is_empty())
        .collect()
}

fn normalize_dependency_name(raw: &str) -> String {
    let mut trimmed = raw.trim();

    if let Some((name, _)) = trimmed.split_once(':') {
        trimmed = name;
    }
    if let Some((name, _)) = trimmed.split_once('(') {
        trimmed = name;
    }
    if let Some((name, _)) = trimmed.split_once('[') {
        trimmed = name;
    }
    if let Some((name, _)) = trimmed.split_once('<') {
        trimmed = name;
    }

    trimmed.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::PackageIndex;

    fn make_package(name: &str, depends: Option<&str>, pre_depends: Option<&str>) -> PackageRecord {
        PackageRecord {
            name: name.to_string(),
            version: "1".to_string(),
            source: "test".to_string(),
            source_base_url: "http://example.invalid".to_string(),
            filename: format!("pool/main/{name}.deb"),
            sha256: "deadbeef".to_string(),
            size: 1,
            depends: depends.map(ToOwned::to_owned),
            pre_depends: pre_depends.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn dependency_parser_strips_constraints_and_arch_markers() {
        let parsed =
            parse_dependency_field("libc6 (>= 2.34), libfoo:any | libbar [amd64], libzstd1");
        assert_eq!(parsed[0], vec!["libc6"]);
        assert_eq!(parsed[1], vec!["libfoo", "libbar"]);
        assert_eq!(parsed[2], vec!["libzstd1"]);
    }

    #[test]
    fn resolver_prefers_first_available_alternative() {
        let mut catalog = PackageCatalog::default();
        catalog.ingest(PackageIndex {
            packages: vec![
                make_package("root", Some("missing | liba, libc6"), None),
                make_package("liba", None, None),
                make_package("libc6", None, None),
            ],
        });

        let roots = vec!["root".to_string()];
        let resolved = resolve_closure(&catalog, &roots).expect("resolution should succeed");

        let names: Vec<String> = resolved.into_iter().map(|pkg| pkg.name).collect();
        assert_eq!(names, vec!["liba", "libc6", "root"]);
    }
}
