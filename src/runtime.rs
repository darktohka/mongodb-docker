use anyhow::{anyhow, bail, Context, Result};
use goblin::elf::Elf;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::config::Arch;

#[derive(Debug)]
pub struct RuntimeSummary {
    pub output_dir: PathBuf,
    pub copied_file_count: usize,
    pub runtime_manifest_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct RuntimeManifest {
    arch: String,
    rootfs_dir: String,
    output_dir: String,
    mongod_path: String,
    interpreter_path: String,
    interpreter_source_path: String,
    include_ca_certs: bool,
    resolved_libraries: Vec<ResolvedLibrary>,
    copied_files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ResolvedLibrary {
    soname: String,
    path: String,
}

#[derive(Debug)]
struct ParsedElf {
    interpreter: Option<String>,
    needed: Vec<String>,
}

pub fn emit_minimal_appdir(
    arch: Arch,
    rootfs_dir: &Path,
    output_dir: &Path,
    manifest_dir: &Path,
    include_ca_certs: bool,
) -> Result<RuntimeSummary> {
    let rootfs_dir = rootfs_dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", rootfs_dir.display()))?;

    let mongod_path = rootfs_dir.join("usr/bin/mongod");
    if !mongod_path.exists() {
        bail!(
            "staged rootfs does not contain mongod at {}",
            mongod_path.display()
        );
    }

    prepare_clean_dir(output_dir)
        .with_context(|| format!("failed to prepare output dir {}", output_dir.display()))?;

    let closure = resolve_elf_closure(arch, &rootfs_dir, &mongod_path)?;
    let mut copied_files = BTreeSet::new();

    for path in &closure.required_paths {
        copy_path_preserving_links(&rootfs_dir, output_dir, path, &mut copied_files)?;
    }

    if closure.interpreter_requested != closure.interpreter_source {
        copy_file_alias(
            &rootfs_dir,
            output_dir,
            &closure.interpreter_requested,
            &closure.interpreter_source,
            &mut copied_files,
        )?;
    }

    if include_ca_certs {
        for rel in ["etc/ssl", "usr/share/ca-certificates"] {
            let src = rootfs_dir.join(rel);
            if src.exists() {
                copy_tree_preserving_links(&rootfs_dir, output_dir, &src, &mut copied_files)?;
            }
        }
    }

    fs::create_dir_all(manifest_dir)
        .with_context(|| format!("failed to create {}", manifest_dir.display()))?;

    let runtime_manifest_path = manifest_dir.join(format!("{}-runtime.json", arch.deb_arch()));
    let resolved_libraries: Vec<ResolvedLibrary> = closure
        .libraries
        .into_iter()
        .map(|(soname, path)| ResolvedLibrary {
            soname,
            path: to_rooted_relative_string(&rootfs_dir, &path),
        })
        .collect();

    let copied_files_vec: Vec<String> = copied_files
        .iter()
        .map(|path| format!("/{}", path.display()))
        .collect();

    let manifest = RuntimeManifest {
        arch: arch.deb_arch().to_string(),
        rootfs_dir: rootfs_dir.display().to_string(),
        output_dir: output_dir.display().to_string(),
        mongod_path: to_rooted_relative_string(&rootfs_dir, &mongod_path),
        interpreter_path: to_rooted_relative_string(&rootfs_dir, &closure.interpreter_requested),
        interpreter_source_path: to_rooted_relative_string(
            &rootfs_dir,
            &closure.interpreter_source,
        ),
        include_ca_certs,
        resolved_libraries,
        copied_files: copied_files_vec,
    };

    let body =
        serde_json::to_string_pretty(&manifest).context("failed to serialize runtime manifest")?;
    fs::write(&runtime_manifest_path, format!("{}\n", body)).with_context(|| {
        format!(
            "failed to write runtime manifest {}",
            runtime_manifest_path.display()
        )
    })?;

    Ok(RuntimeSummary {
        output_dir: output_dir.to_path_buf(),
        copied_file_count: copied_files.len(),
        runtime_manifest_path,
    })
}

#[derive(Debug)]
struct ElfClosure {
    interpreter_requested: PathBuf,
    interpreter_source: PathBuf,
    libraries: BTreeMap<String, PathBuf>,
    required_paths: BTreeSet<PathBuf>,
}

fn resolve_elf_closure(arch: Arch, rootfs_dir: &Path, binary_path: &Path) -> Result<ElfClosure> {
    let search_dirs = library_search_paths(arch, rootfs_dir);

    let mut queue = VecDeque::new();
    queue.push_back(binary_path.to_path_buf());

    let mut visited = BTreeSet::new();
    let mut interpreter_requested: Option<PathBuf> = None;
    let mut interpreter_source: Option<PathBuf> = None;
    let mut libraries = BTreeMap::new();
    let mut required_paths = BTreeSet::new();

    while let Some(path) = queue.pop_front() {
        let canonical = canonical_within_rootfs(rootfs_dir, &path)?;
        if !visited.insert(canonical.clone()) {
            continue;
        }

        required_paths.insert(canonical.clone());
        let parsed = parse_elf_file(&canonical)?;

        if let Some(interpreter) = parsed.interpreter {
            if interpreter_requested.is_none() {
                let (interp_requested_path, interp_source_path) =
                    resolve_interpreter_paths(rootfs_dir, &search_dirs, &interpreter)
                        .with_context(|| {
                            format!(
                                "failed to resolve dynamic linker {} referenced by {}",
                                interpreter,
                                canonical.display()
                            )
                        })?;

                let interp_target = canonical_within_rootfs(rootfs_dir, &interp_source_path)?;
                interpreter_requested = Some(interp_requested_path.clone());
                interpreter_source = Some(interp_source_path.clone());
                required_paths.insert(interp_source_path);
                required_paths.insert(interp_target.clone());
                queue.push_back(interp_target);

                if interp_requested_path.exists() {
                    required_paths.insert(interp_requested_path);
                }
            }
        }

        for needed in parsed.needed {
            if libraries.contains_key(&needed) {
                continue;
            }

            let resolved = resolve_needed_library(rootfs_dir, &canonical, &search_dirs, &needed)
                .ok_or_else(|| {
                    anyhow!(
                        "unresolved shared library {} required by {}",
                        needed,
                        to_rooted_relative_string(rootfs_dir, &canonical)
                    )
                })?;

            let resolved_target = canonical_within_rootfs(rootfs_dir, &resolved)?;
            libraries.insert(needed, resolved.clone());

            required_paths.insert(resolved);
            required_paths.insert(resolved_target.clone());
            queue.push_back(resolved_target);
        }
    }

    let interpreter_requested = interpreter_requested.ok_or_else(|| {
        anyhow!(
            "failed to determine dynamic linker from {}",
            binary_path.display()
        )
    })?;
    let interpreter_source = interpreter_source.ok_or_else(|| {
        anyhow!(
            "failed to determine dynamic linker source from {}",
            binary_path.display()
        )
    })?;

    required_paths.insert(binary_path.to_path_buf());

    Ok(ElfClosure {
        interpreter_requested,
        interpreter_source,
        libraries,
        required_paths,
    })
}

fn resolve_interpreter_paths(
    rootfs_dir: &Path,
    search_dirs: &[PathBuf],
    interpreter: &str,
) -> Result<(PathBuf, PathBuf)> {
    let requested = rootfs_dir.join(interpreter.trim_start_matches('/'));
    if requested.exists() {
        return Ok((requested.clone(), requested));
    }

    let soname = Path::new(interpreter)
        .file_name()
        .ok_or_else(|| anyhow!("dynamic linker path {} has no file name", interpreter))?
        .to_string_lossy()
        .to_string();

    for dir in search_dirs {
        let candidate = dir.join(&soname);
        if candidate.exists() {
            return Ok((requested, candidate));
        }
    }

    bail!(
        "dynamic linker {} not found at requested path {} and no fallback found",
        interpreter,
        requested.display()
    )
}

fn parse_elf_file(path: &Path) -> Result<ParsedElf> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let elf =
        Elf::parse(&bytes).with_context(|| format!("failed to parse ELF {}", path.display()))?;

    let mut needed: Vec<String> = elf.libraries.iter().map(|x| x.to_string()).collect();
    needed.sort();
    needed.dedup();

    Ok(ParsedElf {
        interpreter: elf.interpreter.map(|x| x.to_string()),
        needed,
    })
}

fn library_search_paths(arch: Arch, rootfs_dir: &Path) -> Vec<PathBuf> {
    let multiarch = match arch {
        Arch::Amd64 => "x86_64-linux-gnu",
        Arch::Arm64 => "aarch64-linux-gnu",
    };

    let mut paths = vec![
        rootfs_dir.join("lib"),
        rootfs_dir.join("lib64"),
        rootfs_dir.join("usr/lib"),
        rootfs_dir.join(format!("lib/{}", multiarch)),
        rootfs_dir.join(format!("usr/lib/{}", multiarch)),
        rootfs_dir.join("usr/local/lib"),
    ];

    paths.retain(|path| path.exists());
    paths
}

fn resolve_needed_library(
    rootfs_dir: &Path,
    current: &Path,
    search_dirs: &[PathBuf],
    soname: &str,
) -> Option<PathBuf> {
    if soname.contains('/') {
        let candidate = rootfs_dir.join(soname.trim_start_matches('/'));
        if candidate.exists() {
            return Some(candidate);
        }
        return None;
    }

    if let Some(parent) = current.parent() {
        let candidate = parent.join(soname);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    for dir in search_dirs {
        let candidate = dir.join(soname);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

fn prepare_clean_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove existing {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    Ok(())
}

fn copy_tree_preserving_links(
    rootfs_dir: &Path,
    output_dir: &Path,
    source: &Path,
    copied_files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to stat {}", source.display()))?;

    if metadata.file_type().is_dir() {
        let rel = source
            .strip_prefix(rootfs_dir)
            .with_context(|| format!("{} is outside rootfs", source.display()))?;
        let dest = output_dir.join(rel);
        fs::create_dir_all(&dest)
            .with_context(|| format!("failed to create {}", dest.display()))?;

        for entry in
            fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry in {}", source.display()))?;
            copy_tree_preserving_links(rootfs_dir, output_dir, &entry.path(), copied_files)?;
        }
        return Ok(());
    }

    copy_path_preserving_links(rootfs_dir, output_dir, source, copied_files)
}

fn copy_path_preserving_links(
    rootfs_dir: &Path,
    output_dir: &Path,
    source: &Path,
    copied_files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let rel = source
        .strip_prefix(rootfs_dir)
        .with_context(|| format!("{} is outside rootfs", source.display()))?
        .to_path_buf();

    if copied_files.contains(&rel) {
        return Ok(());
    }

    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to stat {}", source.display()))?;
    let dest = output_dir.join(&rel);

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    remove_existing_path(&dest)?;

    if metadata.file_type().is_symlink() {
        let link_target = fs::read_link(source)
            .with_context(|| format!("failed to read link {}", source.display()))?;
        symlink(&link_target, &dest).with_context(|| {
            format!(
                "failed to create symlink {} -> {}",
                dest.display(),
                link_target.display()
            )
        })?;

        copied_files.insert(rel);

        let target_abs = if link_target.is_absolute() {
            rootfs_dir.join(link_target.strip_prefix("/").unwrap_or(&link_target))
        } else {
            source
                .parent()
                .ok_or_else(|| anyhow!("{} has no parent", source.display()))?
                .join(link_target)
        };

        if !target_abs.exists() {
            bail!(
                "symlink {} points to missing target {}",
                source.display(),
                target_abs.display()
            );
        }

        let target_canonical = canonical_within_rootfs(rootfs_dir, &target_abs)?;
        if fs::symlink_metadata(&target_canonical)
            .with_context(|| format!("failed to stat {}", target_canonical.display()))?
            .file_type()
            .is_dir()
        {
            copy_tree_preserving_links(rootfs_dir, output_dir, &target_canonical, copied_files)
        } else {
            copy_path_preserving_links(rootfs_dir, output_dir, &target_canonical, copied_files)
        }
    } else if metadata.file_type().is_file() {
        fs::copy(source, &dest).with_context(|| {
            format!("failed to copy {} to {}", source.display(), dest.display())
        })?;
        fs::set_permissions(&dest, metadata.permissions())
            .with_context(|| format!("failed to set permissions on {}", dest.display()))?;
        copied_files.insert(rel);
        Ok(())
    } else {
        Ok(())
    }
}

fn copy_file_alias(
    rootfs_dir: &Path,
    output_dir: &Path,
    alias_path: &Path,
    source_path: &Path,
    copied_files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let rel = alias_path
        .strip_prefix(rootfs_dir)
        .with_context(|| format!("{} is outside rootfs", alias_path.display()))?
        .to_path_buf();

    if copied_files.contains(&rel) {
        return Ok(());
    }

    let source = canonical_within_rootfs(rootfs_dir, source_path)?;
    let source_metadata =
        fs::metadata(&source).with_context(|| format!("failed to stat {}", source.display()))?;
    if !source_metadata.is_file() {
        bail!(
            "cannot create alias {} from non-file source {}",
            alias_path.display(),
            source.display()
        );
    }

    let dest = output_dir.join(&rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    remove_existing_path(&dest)?;
    fs::copy(&source, &dest)
        .with_context(|| format!("failed to copy {} to {}", source.display(), dest.display()))?;
    fs::set_permissions(&dest, source_metadata.permissions())
        .with_context(|| format!("failed to set permissions on {}", dest.display()))?;

    copied_files.insert(rel);
    Ok(())
}

fn remove_existing_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };

    if metadata.file_type().is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
    }
}

fn canonical_within_rootfs(rootfs_dir: &Path, path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))?;
    if !canonical.starts_with(rootfs_dir) {
        bail!(
            "path {} resolves outside rootfs {}",
            canonical.display(),
            rootfs_dir.display()
        );
    }
    Ok(canonical)
}

fn to_rooted_relative_string(rootfs_dir: &Path, path: &Path) -> String {
    match path.strip_prefix(rootfs_dir) {
        Ok(rel) => format!("/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}
