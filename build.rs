use std::path::{Path, PathBuf};
use std::process::Command;

fn get_git_dir() -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let path = PathBuf::from(s.trim());
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

fn watch_git_repo(git_dir: &Path) {
    let head = git_dir.join("HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(content) = std::fs::read_to_string(&head) {
            if let Some(ref_rel) = content.strip_prefix("ref: ") {
                let ref_path = git_dir.join(ref_rel.trim());
                if ref_path.exists() {
                    println!("cargo:rerun-if-changed={}", ref_path.display());
                }
            }
        }
    }
    let index = git_dir.join("index");
    if index.exists() {
        println!("cargo:rerun-if-changed={}", index.display());
    }
}

fn get_git_describe() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=FINCH_VERSION_OVERRIDE");

    if let Some(git_dir) = get_git_dir() {
        watch_git_repo(&git_dir);
    }

    let pkg_version = env!("CARGO_PKG_VERSION");
    let git_describe = get_git_describe();

    let version_string = if let Ok(override_ver) = std::env::var("FINCH_VERSION_OVERRIDE") {
        override_ver
    } else if let Some(ref describe) = git_describe {
        format!("{pkg_version} ({describe})")
    } else {
        pkg_version.to_string()
    };

    println!("cargo:rustc-env=FINCH_VERSION_STRING={version_string}");
    if let Some(ref describe) = git_describe {
        println!("cargo:rustc-env=FINCH_GIT_DESCRIBE={describe}");
    } else {
        println!("cargo:rustc-env=FINCH_GIT_DESCRIBE=");
    }
}
