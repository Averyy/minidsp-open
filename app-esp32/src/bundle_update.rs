use std::io::Write;
use std::path::Path;

const INCOMPLETE: &str = ".update-incomplete";

/// Sizes are rounded to LittleFS's allocation block before planning.
pub struct PendingFile<'a> {
    pub name: &'a str,
    pub old_size: u64,
    pub new_size: u64,
}

#[derive(Debug, PartialEq)]
pub struct ReplacementPlan {
    /// Indices into the caller's pending files, in download order.
    pub order: Vec<usize>,
    pub delete_before_download: bool,
}

pub fn replacement_plan(
    files: &[PendingFile<'_>],
    available: u64,
    margin: u64,
) -> Result<ReplacementPlan, String> {
    fn blocks(size: u64) -> Result<i128, String> {
        size.checked_add(4095)
            .map(|size| i128::from(size / 4096) * 4096)
            .ok_or_else(|| "manifest sizes overflow".into())
    }
    let sizes: Vec<_> = files
        .iter()
        .map(|file| Ok((blocks(file.old_size)?, blocks(file.new_size)?)))
        .collect::<Result<_, String>>()?;
    let mut order: Vec<_> = (0..files.len()).collect();
    // Shrinking files release space for growing/new files. The UI entry point
    // always publishes last, after all of its new assets have verified.
    order.sort_by_key(|&i| {
        (
            files[i].name == "ui/index.html",
            sizes[i].1 - sizes[i].0,
            files[i].name,
        )
    });
    let fits = |delete_first| {
        let mut free = i128::from(available);
        for &i in &order {
            let (old, new) = sizes[i];
            if delete_first {
                free += old;
            }
            free -= new;
            if free < i128::from(margin) {
                return false;
            }
            if !delete_first {
                free += old;
            }
        }
        free >= i128::from(margin)
    };
    let delete_before_download = if fits(false) {
        false
    } else if fits(true) {
        true
    } else {
        return Err("insufficient space for the new bundle".into());
    };
    Ok(ReplacementPlan {
        order,
        delete_before_download,
    })
}

pub fn installed_version(root: &Path) -> Option<String> {
    if root.join(INCOMPLETE).exists() || !root.join("catalogue.bin").is_file() {
        return None;
    }
    std::fs::read_to_string(root.join("version.txt"))
        .ok()
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
}

pub fn safe_filename(filename: &str) -> bool {
    !filename.is_empty()
        && !filename.contains(['\\', '\0', '?', '#', '%'])
        && filename
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.'))
        && !filename.ends_with(".part")
}

pub fn begin(root: &Path) -> std::io::Result<()> {
    let file = std::fs::File::create(root.join(INCOMPLETE))?;
    file.sync_all()
}

pub fn discard_downloads(root: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            discard_downloads(&path)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "part")
        {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub fn commit(root: &Path, version: &str) -> std::io::Result<()> {
    let temp = root.join("version.txt.part");
    let mut file = std::fs::File::create(&temp)?;
    writeln!(file, "{version}")?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, root.join("version.txt"))?;
    std::fs::remove_file(root.join(INCOMPLETE))
}

/// Delete files under `root` that the committed bundle no longer lists
/// (`keep` holds manifest paths relative to `root`), then empty directories.
/// The commit marker and journal are never removed. Returns the file count.
pub fn reclaim_unreferenced(
    root: &Path,
    keep: &std::collections::HashSet<&str>,
) -> std::io::Result<usize> {
    fn walk(
        dir: &Path,
        prefix: &str,
        keep: &std::collections::HashSet<&str>,
    ) -> std::io::Result<usize> {
        let mut removed = 0;
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let relative = format!("{prefix}{name}");
            if path.is_dir() {
                removed += walk(&path, &format!("{relative}/"), keep)?;
                if std::fs::read_dir(&path)?.next().is_none() {
                    std::fs::remove_dir(&path)?;
                }
            } else if !keep.contains(relative.as_str())
                && relative != "version.txt"
                && relative != INCOMPLETE
            {
                std::fs::remove_file(&path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
    walk(root, "", keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_bundle_never_advertises_a_committed_version() {
        let root = std::env::temp_dir().join(format!("esp-bundle-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("catalogue.bin"), "old catalogue").unwrap();
        std::fs::write(root.join("version.txt"), "old").unwrap();
        assert_eq!(installed_version(&root).as_deref(), Some("old"));
        begin(&root).unwrap();
        assert_eq!(installed_version(&root), None);
        std::fs::write(root.join("obsolete.js.part"), "interrupted download").unwrap();
        discard_downloads(&root).unwrap();
        assert!(!root.join("obsolete.js.part").exists());
        assert_eq!(installed_version(&root), None);
        assert_eq!(
            std::fs::read_to_string(root.join("catalogue.bin")).unwrap(),
            "old catalogue"
        );
        // A reset after updating version.txt but before clearing the journal must retry.
        std::fs::write(root.join("version.txt"), "new").unwrap();
        assert_eq!(installed_version(&root), None);
        commit(&root, "new").unwrap();
        assert_eq!(installed_version(&root).as_deref(), Some("new"));
        std::fs::remove_file(root.join("catalogue.bin")).unwrap();
        assert_eq!(installed_version(&root), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reclaim_removes_only_files_the_bundle_no_longer_lists() {
        let root = std::env::temp_dir().join(format!("esp-reclaim-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("ui/static/old")).unwrap();
        std::fs::create_dir_all(root.join("ui/static/js")).unwrap();
        for file in [
            "catalogue.bin",
            "version.txt",
            "ui/index.html",
            "ui/static/js/main.new.js",
            "ui/static/js/main.old.js",
            "ui/static/old/gone.css",
        ] {
            std::fs::write(root.join(file), "x").unwrap();
        }
        let keep = ["catalogue.bin", "ui/index.html", "ui/static/js/main.new.js"]
            .into_iter()
            .collect();
        assert_eq!(reclaim_unreferenced(&root, &keep).unwrap(), 2);
        assert!(root.join("version.txt").exists());
        assert!(root.join("ui/static/js/main.new.js").exists());
        assert!(!root.join("ui/static/js/main.old.js").exists());
        assert!(!root.join("ui/static/old").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_names_cannot_overwrite_journal_or_escape_storage() {
        for name in [
            "",
            "../a",
            "/a",
            "a/../b",
            "a//b",
            ".update-incomplete",
            "a.part",
            "a?b",
            "a%2fb",
        ] {
            assert!(!safe_filename(name), "{name}");
        }
        assert!(safe_filename("static/js/main.123.js.gz"));
        assert!(safe_filename("version.txt"));
    }
}
