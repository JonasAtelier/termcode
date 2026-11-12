//! Repository trust, as VS Code's Workspace Trust for git. A repository's own config
//! can make git run a program the moment git reads it (`core.fsmonitor`, filters,
//! `diff.external`, …), and termcode reads it unasked: on open, on every refresh, for
//! diffs and blame. So a repository from an unpacked archive or a shared disk could run
//! anything just by being opened. Such a repository gets no git command until the user
//! trusts it; trusting stores those settings, and a change asks again.
//!
//! Programs that run only on an action the user takes (hooks on commit, ssh and
//! credential helpers on pull and push, gpg on a signed commit) are left alone: that is
//! what running git in a shell does too. The user's own (global, system) config is
//! theirs and always trusted.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether git would run `value` as a program for `key` while only reading the
/// repository (status, diff, log, blame). A subsection may hold dots, so only the ends of
/// the key are matched.
fn runs_program(key: &str, value: &str) -> bool {
    let k = key.to_ascii_lowercase();
    let parts: Vec<&str> = k.split('.').collect();
    let (first, last, sub) = (parts[0], parts[parts.len() - 1], parts.len() > 2);
    match (first, last) {
        // true/false is git's built-in daemon, not a program.
        ("core", "fsmonitor") => !matches!(value.to_ascii_lowercase().as_str(), "" | "true" | "false" | "yes" | "no" | "on" | "off" | "1" | "0"),
        ("core", "pager") => true,
        ("diff", "external") => !sub,
        ("diff", "textconv" | "command") | ("filter", "clean" | "smudge" | "process") => sub,
        ("pager", _) => parts.len() == 2,
        _ => false,
    }
}

/// `key=value` for each setting in the repository's own config (local and worktree,
/// includes followed) that runs a program. Reading config runs nothing.
pub fn risky(repo: &Path) -> Vec<String> {
    let Ok(out) = Command::new("git").arg("-C").arg(repo).args(["config", "--list", "--show-scope", "--includes", "-z"]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.split('\0');
    let mut found = Vec::new();
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        if matches!(scope, "local" | "worktree") && runs_program(key, value) {
            found.push(format!("{key}={value}"));
        }
    }
    found
}

/// Where trusted repositories are kept; in tests one file per test thread, so parallel
/// tests never see each other's.
pub(crate) fn store() -> PathBuf {
    if cfg!(test) {
        let thread = format!("{:?}", std::thread::current().id()).replace(|c: char| !c.is_ascii_alphanumeric(), "");
        std::env::temp_dir().join(format!("termcode-trust-{}-{thread}.json", std::process::id()))
    } else {
        crate::data_dir(true).join("trusted.json")
    }
}

fn load() -> BTreeMap<String, Vec<String>> {
    fs::read(store()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Trusted with exactly these settings.
pub fn trusted(repo: &Path, risky: &[String]) -> bool {
    load().get(&*repo.to_string_lossy()).is_some_and(|s| s == risky)
}

pub fn trust(repo: &Path, risky: &[String]) -> std::io::Result<()> {
    let mut all = load();
    all.insert(repo.to_string_lossy().into_owned(), risky.to_vec());
    let path = store();
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&all).unwrap_or_default())?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_that_run_programs_are_found_in_the_repo_config_only() {
        for (k, v) in [
            ("core.fsmonitor", "/tmp/x.sh"),
            ("core.pager", "less"),
            ("diff.external", "x"),
            ("diff.pdf.textconv", "pdftotext"),
            ("diff.my.tool.command", "x"),
            ("filter.lfs.process", "git-lfs filter-process"),
            ("filter.x.clean", "x"),
            ("pager.log", "less"),
        ] {
            assert!(runs_program(k, v), "{k}");
        }
        // Built-in values, plain settings, and programs only an action of the user runs.
        for (k, v) in [
            ("core.fsmonitor", "true"),
            ("core.fsmonitor", "false"),
            ("user.name", "x"),
            ("diff.renames", "true"),
            ("core.hooksPath", ".husky"),
            ("core.sshCommand", "ssh -i k"),
            ("credential.https://h.example.helper", "store"),
            ("gpg.program", "gpg2"),
        ] {
            assert!(!runs_program(k, v), "{k}={v}");
        }
        let d = std::env::temp_dir().join(format!("termcode-trust-repo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&d).args(a).output().unwrap().status.success());
        fs::create_dir_all(&d).unwrap();
        git(&["init", "-q"]);
        assert!(risky(&d).is_empty(), "a fresh repository runs nothing");
        git(&["config", "core.fsmonitor", "/tmp/evil.sh"]);
        git(&["config", "filter.x.clean", "evil"]);
        let r = risky(&d);
        assert_eq!(r, ["core.fsmonitor=/tmp/evil.sh", "filter.x.clean=evil"]);
        assert!(!trusted(&d, &r));
        trust(&d, &r).unwrap();
        assert!(trusted(&d, &r));
        git(&["config", "diff.external", "evil2"]);
        assert!(!trusted(&d, &risky(&d)), "a new program asks again");
        let _ = fs::remove_file(store());
        fs::remove_dir_all(&d).unwrap();
    }
}
