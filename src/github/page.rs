//! A repository's page in the editor area, as github.com shows one. In the middle: the
//! main page (the branch's latest commit, a folder's files each with its last commit, its
//! README rendered), or a file's code (read-only, the editor's own view: it scrolls,
//! selects, finds and copies as any). On the right: a button back to the main page, the
//! repository's files as a tree, and under it the shown file's outline. The header's
//! branch switches the branch. All of it comes from `gh api graphql`. The tab is
//! `repo:owner/name`; while a file shows, its text is that tab's document.

use super::*;
use crate::editor::{Buffer, Pos, View};
use crate::icons::{file_icon, folder_icon};
use crate::panel::Doc;
use crate::ui::{fill, fit};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::collections::{HashMap, HashSet};

/// Entries asked for their last commit (one query each would be slow; one query with an
/// alias per entry is quick up to here).
const LAST_COMMITS: usize = 100;
/// The right pane's width: a third of the page, within these.
const SIDE_MIN: u16 = 24;
const SIDE_MAX: u16 = 42;

/// A folder of a repository at a branch's head.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dir {
    /// The repository's default branch.
    pub default_branch: String,
    /// The branch's latest commit: author, headline, short hash, time.
    pub head: Option<(String, String, String, i64)>,
    pub entries: Vec<Entry>,
    /// The folder's README: its name and text.
    pub readme: Option<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    /// The last commit that touched it: headline, time.
    pub last: Option<(String, i64)>,
}

/// The page of one repository: the branch shown (None: the default one), the folders
/// listed (by path, "" the root) and those being fetched, the folders open in the tree,
/// the folder the main page lists, the file shown instead (and the files fetched), the
/// tree's scroll, the branches once asked for.
#[derive(Default)]
pub struct Page {
    pub branch: Option<String>,
    pub dirs: HashMap<String, Result<Dir, String>>,
    asked: HashSet<String>,
    pub open: HashSet<String>,
    pub folder: String,
    pub file: Option<String>,
    files: HashMap<String, Result<Vec<String>, String>>,
    files_asked: HashSet<String>,
    pub tree_off: usize,
    branches_asked: bool,
    /// The main page's insights (the default branch's), once asked for.
    pub stats: Option<Result<Stats, String>>,
    stats_asked: bool,
    /// A README rendered for a width: (folder, width, its lines and pictures).
    readme: Option<(String, u16, crate::mdpreview::Rendered)>,
    /// Its pictures asked for (fetched or failed: never asked again).
    pics_asked: HashSet<PathBuf>,
    /// The main page's About, lists and traffic (of a revision), and the revision asked.
    pub about: Option<(String, Result<About, String>)>,
    about_asked: Option<String>,
}

impl Page {
    /// The revision its queries read: the branch, else the default branch's head.
    fn rev(&self) -> String {
        self.branch.clone().unwrap_or_else(|| "HEAD".into())
    }

    /// The branch's name as shown.
    pub fn branch_name(&self) -> String {
        self.branch.clone().or_else(|| self.dirs.get("").and_then(|d| d.as_ref().ok()).map(|d| d.default_branch.clone())).unwrap_or_default()
    }
}

/// A repository's insights for its main page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub stars: u64,
    pub forks: u64,
    pub watchers: u64,
    /// Pull requests open, merged, closed; issues open, closed.
    pub prs: [u64; 3],
    pub issues: [u64; 2],
    /// Languages by size, with GitHub's colour for each.
    pub languages: Vec<(String, u64, Color)>,
    /// Commits a day for the past year, a week a row, oldest first.
    pub weeks: Vec<[u64; 7]>,
    /// Commits by weekday (Sunday first) and hour.
    pub punch: Vec<[u64; 24]>,
    /// Lines added and removed a week, oldest first.
    pub churn: Vec<(u64, u64)>,
    /// The top contributors and their commits.
    pub people: Vec<(String, u64)>,
}

/// The main page's facts besides the files and insights: GitHub's About, the shown
/// revision's commits and checks, the counts of branches, tags and releases, the latest
/// release, the open pull requests and issues, the latest runs, and the past two weeks'
/// traffic (None without push access).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct About {
    pub description: String,
    pub homepage: String,
    pub topics: Vec<String>,
    pub license: String,
    /// "Public", "Private" or "Internal"; the repository it forks; archived.
    pub visibility: String,
    pub parent: String,
    pub archived: bool,
    /// Its size in KB (GitHub's diskUsage), when created and last pushed.
    pub size_kb: u64,
    pub created: i64,
    pub pushed: i64,
    pub commits: u64,
    pub checks: Checks,
    pub branches: u64,
    pub tags: u64,
    pub releases: u64,
    /// The latest release: its name, tag, time, a prerelease, its page.
    pub release: Option<(String, String, i64, bool, String)>,
    pub prs: Vec<Pr>,
    /// Each issue with its labels (name, colour).
    pub issues: Vec<(Issue, Vec<(String, Color)>)>,
    pub runs: Vec<Run>,
    pub views: Option<Traffic>,
    pub clones: Option<Traffic>,
}

/// Two weeks of views or clones: all, unique, and each day's (date, all, unique).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Traffic {
    pub all: u64,
    pub unique: u64,
    pub days: Vec<(String, u64, u64)>,
}

/// What a click on the page does.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A folder in the tree: open or close it.
    Folder(String),
    /// A folder in the main page's list: list it there (and open it in the tree).
    Into(String),
    /// A file (in the tree or the list): show its code.
    File(String),
    /// A symbol of the outline: its line.
    Symbol(usize),
    /// The main page button and the repository's name: the main page of the root.
    Main,
    /// The header's branch: pick another.
    Branches,
    /// A pull request, issue, run, release or the homepage: in the browser.
    Url(String),
}

pub fn tab_key(repo: &str) -> String {
    format!("repo:{repo}")
}

/// A JSON string is a GraphQL string literal.
fn lit(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

fn is_readme(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "readme.md" | "readme" | "readme.markdown" | "readme.txt" | "readme.rst")
}

fn repo_q(repo: &str) -> Result<String, String> {
    let (owner, name) = repo.split_once('/').ok_or("not owner/name")?;
    Ok(format!("repository(owner: {}, name: {})", lit(owner), lit(name)))
}

/// Folder `path` of `repo` ("owner/name") at revision `rev` (a branch, or HEAD).
pub(super) fn load_dir(bin: &Path, root: &Path, repo: &str, rev: &str, path: &str) -> Result<Dir, String> {
    let repo_q = repo_q(repo)?;
    let one = format!(
        "query=query {{ {repo_q} {{ defaultBranchRef {{ name }} head: object(expression: {}) {{ ... on Commit {{ oid messageHeadline \
         committedDate author {{ name user {{ login }} }} }} }} object(expression: {}) {{ ... on Tree {{ entries {{ name type path }} }} }} }} }}",
        lit(rev),
        lit(&format!("{rev}:{path}"))
    );
    let v = gh_json(bin, root, &["api", "graphql", "-f", &one]).map_err(Fail::text)?;
    let r = &v["data"]["repository"];
    if r.is_null() {
        return Err(v["errors"][0]["message"].as_str().unwrap_or("no such repository").to_string());
    }
    if r["defaultBranchRef"].is_null() {
        return Err("an empty repository".into());
    }
    let c = &r["head"];
    let author = c["author"]["user"]["login"].as_str().or(c["author"]["name"].as_str()).unwrap_or("").to_string();
    let oid = str_of(c, "oid");
    let head = (!c.is_null()).then(|| (author, str_of(c, "messageHeadline"), oid[..oid.len().min(7)].to_string(), iso_time(&str_of(c, "committedDate"))));
    let Some(list) = r["object"]["entries"].as_array() else { return Err(format!("{path}: no such folder on {rev}")) };
    let mut entries: Vec<Entry> =
        list.iter().map(|e| Entry { name: str_of(e, "name"), path: str_of(e, "path"), dir: e["type"] == "tree", last: None }).collect();
    // Folders first, then by name, as GitHub lists them.
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let readme = entries.iter().find(|e| !e.dir && is_readme(&e.name)).map(|e| (e.name.clone(), e.path.clone()));
    // Each entry's last commit, and the README's text, in one more query; without them
    // the list still shows.
    let hist: String = entries
        .iter()
        .take(LAST_COMMITS)
        .enumerate()
        .map(|(i, e)| format!("e{i}: history(first: 1, path: {}) {{ nodes {{ messageHeadline committedDate }} }} ", lit(&e.path)))
        .collect();
    let rd = readme.as_ref().map_or(String::new(), |(_, p)| format!("readme: object(expression: {}) {{ ... on Blob {{ text }} }}", lit(&format!("{rev}:{p}"))));
    let two = format!("query=query {{ {repo_q} {{ object(expression: {}) {{ ... on Commit {{ {hist} }} }} {rd} }} }}", lit(rev));
    let mut text = None;
    if let Ok(v) = gh_json(bin, root, &["api", "graphql", "-f", &two]) {
        let r = &v["data"]["repository"];
        for (i, e) in entries.iter_mut().take(LAST_COMMITS).enumerate() {
            let n = &r["object"][format!("e{i}")]["nodes"][0];
            if !n.is_null() {
                e.last = Some((str_of(n, "messageHeadline"), iso_time(&str_of(n, "committedDate"))));
            }
        }
        text = r["readme"]["text"].as_str().map(str::to_string);
    }
    Ok(Dir { default_branch: str_of(&r["defaultBranchRef"], "name"), head, entries, readme: readme.zip(text).map(|((n, _), t)| (n, t)) })
}

/// File `path` of `repo` at revision `rev`, as text.
pub(super) fn load_blob(bin: &Path, root: &Path, repo: &str, rev: &str, path: &str) -> Result<String, String> {
    let q = format!("query=query {{ {} {{ object(expression: {}) {{ ... on Blob {{ text isBinary byteSize }} }} }} }}", repo_q(repo)?, lit(&format!("{rev}:{path}")));
    let v = gh_json(bin, root, &["api", "graphql", "-f", &q]).map_err(Fail::text)?;
    let o = &v["data"]["repository"]["object"];
    if o.is_null() {
        return Err(format!("{path}: not on {rev}"));
    }
    if o["isBinary"].as_bool() == Some(true) {
        return Err(format!("a binary file ({} bytes): open it on GitHub", o["byteSize"]));
    }
    o["text"].as_str().map(str::to_string).ok_or_else(|| "too large to show here: open it on GitHub".into())
}

/// The repository's branches, newest first.
pub(super) fn load_branches(bin: &Path, root: &Path, repo: &str) -> Result<Vec<String>, String> {
    let q = format!(
        "query=query {{ {} {{ refs(refPrefix: \"refs/heads/\", first: 100, orderBy: {{field: TAG_COMMIT_DATE, direction: DESC}}) {{ nodes {{ name }} }} }} }}",
        repo_q(repo)?
    );
    let v = gh_json(bin, root, &["api", "graphql", "-f", &q]).map_err(Fail::text)?;
    let nodes = v["data"]["repository"]["refs"]["nodes"].as_array().ok_or("no branches")?;
    Ok(nodes.iter().map(|n| str_of(n, "name")).collect())
}

/// A statistics endpoint's answer: GitHub works some out on the first ask (202, nothing
/// yet), so it is asked again a few times.
fn stats_get(bin: &Path, root: &Path, path: &str) -> Value {
    for k in 0..5 {
        if let Ok(v) = gh_json(bin, root, &["api", path]) {
            if v.as_array().is_some_and(|a| !a.is_empty()) {
                return v;
            }
        }
        if k < 4 {
            std::thread::sleep(STATS_AGAIN);
        }
    }
    Value::Null
}

/// `repo`'s insights: counts and languages in one query, the activity charts from the
/// statistics endpoints (asked at once, each waiting for GitHub as it needs).
pub(super) fn load_stats(bin: &Path, root: &Path, repo: &str) -> Result<Stats, String> {
    let q = format!(
        "query=query {{ {} {{ stargazerCount forkCount watchers {{ totalCount }} open: pullRequests(states: OPEN) {{ totalCount }} \
         merged: pullRequests(states: MERGED) {{ totalCount }} closed: pullRequests(states: CLOSED) {{ totalCount }} \
         openIssues: issues(states: OPEN) {{ totalCount }} closedIssues: issues(states: CLOSED) {{ totalCount }} \
         languages(first: 8, orderBy: {{field: SIZE, direction: DESC}}) {{ edges {{ size node {{ name color }} }} }} }} }}",
        repo_q(repo)?
    );
    let v = gh_json(bin, root, &["api", "graphql", "-f", &q]).map_err(Fail::text)?;
    let r = &v["data"]["repository"];
    if r.is_null() {
        return Err(v["errors"][0]["message"].as_str().unwrap_or("no such repository").to_string());
    }
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let languages = r["languages"]["edges"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            let name = str_of(&e["node"], "name");
            let color = e["node"]["color"].as_str().and_then(hex).unwrap_or_else(|| lang_color(&name));
            (name, n(&e["size"]), color)
        })
        .collect();
    let (weeks, punch, churn, people) = std::thread::scope(|s| {
        let get = |path: String| s.spawn(move || stats_get(bin, root, &path));
        let (a, p, c, w) = (
            get(format!("repos/{repo}/stats/commit_activity")),
            get(format!("repos/{repo}/stats/punch_card")),
            get(format!("repos/{repo}/stats/code_frequency")),
            get(format!("repos/{repo}/contributors?per_page=5")),
        );
        let join = |h: std::thread::ScopedJoinHandle<'_, Value>| h.join().unwrap_or(Value::Null);
        (join(a), join(p), join(c), join(w))
    });
    let weeks = weeks.as_array().into_iter().flatten().map(|w| std::array::from_fn(|d| n(&w["days"][d]))).collect();
    let mut by_day = vec![[0u64; 24]; 7];
    for x in punch.as_array().into_iter().flatten() {
        let (d, h) = (n(&x[0]) as usize, n(&x[1]) as usize);
        if d < 7 && h < 24 {
            by_day[d][h] = n(&x[2]);
        }
    }
    let churn = churn.as_array().into_iter().flatten().map(|x| (n(&x[1]), x[2].as_i64().unwrap_or(0).unsigned_abs())).collect();
    let people = people.as_array().into_iter().flatten().map(|p| (str_of(p, "login"), n(&p["contributions"]))).collect();
    Ok(Stats {
        stars: n(&r["stargazerCount"]),
        forks: n(&r["forkCount"]),
        watchers: n(&r["watchers"]["totalCount"]),
        prs: [n(&r["open"]["totalCount"]), n(&r["merged"]["totalCount"]), n(&r["closed"]["totalCount"])],
        issues: [n(&r["openIssues"]["totalCount"]), n(&r["closedIssues"]["totalCount"])],
        languages,
        weeks,
        punch: by_day,
        churn,
        people,
    })
}

/// A colour written "#rrggbb" or "rrggbb".
fn hex(c: &str) -> Option<Color> {
    let c = c.trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(c.get(i..i + 2)?, 16).ok();
    Some(Color::Rgb(b(0)?, b(2)?, b(4)?))
}

/// `repo`'s About and the shown revision's counts in one query; its lists and traffic
/// beside it, each left out when it fails (traffic needs push access).
pub(super) fn load_about(bin: &Path, root: &Path, repo: &str, rev: &str) -> Result<About, String> {
    let q = format!(
        "query=query {{ {} {{ description homepageUrl visibility isArchived parent {{ nameWithOwner }} diskUsage createdAt pushedAt \
         licenseInfo {{ spdxId name }} repositoryTopics(first: 20) {{ nodes {{ topic {{ name }} }} }} \
         branches: refs(refPrefix: \"refs/heads/\", first: 1) {{ totalCount }} tags: refs(refPrefix: \"refs/tags/\", first: 1) {{ totalCount }} \
         releases(first: 1, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ totalCount nodes {{ name tagName publishedAt isPrerelease url }} }} \
         rev: object(expression: {}) {{ ... on Commit {{ history(first: 1) {{ totalCount }} statusCheckRollup {{ state }} }} }} }} }}",
        repo_q(repo)?,
        lit(rev)
    );
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    std::thread::scope(|s| {
        let get = |args: Vec<String>| s.spawn(move || gh_json(bin, root, &args.iter().map(String::as_str).collect::<Vec<_>>()).ok());
        let list = |what: &str, fields: &str| get(["--repo", repo, "--limit", "5", "--json", fields].iter().fold(vec![what.to_string(), "list".into()], |mut v, a| {
            v.push(a.to_string());
            v
        }));
        let prs_h = list("pr", "number,title,author,isDraft,headRefName,statusCheckRollup,updatedAt,url");
        let issues_h = list("issue", "number,title,author,updatedAt,url,labels");
        let runs_h = list("run", "databaseId,displayTitle,workflowName,headBranch,status,conclusion,updatedAt,url");
        let views_h = get(vec!["api".into(), format!("repos/{repo}/traffic/views")]);
        let clones_h = get(vec!["api".into(), format!("repos/{repo}/traffic/clones")]);
        let v = gh_json(bin, root, &["api", "graphql", "-f", &q]).map_err(Fail::text)?;
        let join = |h: std::thread::ScopedJoinHandle<'_, Option<Value>>| h.join().ok().flatten();
        let (prs_v, issues_v, runs_v, views_v, clones_v) = (join(prs_h), join(issues_h), join(runs_h), join(views_h), join(clones_h));
        let r = &v["data"]["repository"];
        if r.is_null() {
            return Err(v["errors"][0]["message"].as_str().unwrap_or("no such repository").to_string());
        }
        let checks = match r["rev"]["statusCheckRollup"]["state"].as_str().unwrap_or("") {
            "SUCCESS" => Checks::Pass,
            "FAILURE" | "ERROR" => Checks::Fail,
            "PENDING" | "EXPECTED" => Checks::Pending,
            _ => Checks::None,
        };
        let rel = &r["releases"]["nodes"][0];
        let issues = issues_v.map_or_else(Vec::new, |v| {
            let labels = v.as_array().into_iter().flatten().map(|i| {
                i["labels"].as_array().into_iter().flatten().map(|l| (str_of(l, "name"), hex(&str_of(l, "color")).unwrap_or(P.dim))).collect()
            });
            super::issues(&v, repo).into_iter().zip(labels).collect()
        });
        let traffic = |v: Option<Value>, k: &str| {
            let v = v.filter(|v| v["count"].is_u64())?;
            // "2026-11-10T00:00:00Z" -> "Nov 10".
            let date = |t: &str| {
                let m = t.get(5..7).and_then(|m| m.parse::<usize>().ok()).filter(|m| (1..=12).contains(m)).unwrap_or(1);
                format!("{} {}", super::draw::MONTHS[m - 1], t.get(8..10).unwrap_or("").trim_start_matches('0'))
            };
            let days = v[k].as_array().into_iter().flatten().map(|d| (date(&str_of(d, "timestamp")), n(&d["count"]), n(&d["uniques"]))).collect();
            Some(Traffic { all: n(&v["count"]), unique: n(&v["uniques"]), days })
        };
        Ok(About {
            description: str_of(r, "description"),
            homepage: str_of(r, "homepageUrl"),
            topics: r["repositoryTopics"]["nodes"].as_array().into_iter().flatten().map(|t| str_of(&t["topic"], "name")).collect(),
            license: r["licenseInfo"]["spdxId"].as_str().filter(|s| *s != "NOASSERTION").map_or_else(|| str_of(&r["licenseInfo"], "name"), str::to_string),
            visibility: str_of(r, "visibility"),
            parent: str_of(&r["parent"], "nameWithOwner"),
            archived: r["isArchived"].as_bool().unwrap_or(false),
            size_kb: n(&r["diskUsage"]),
            created: iso_time(&str_of(r, "createdAt")),
            pushed: iso_time(&str_of(r, "pushedAt")),
            commits: n(&r["rev"]["history"]["totalCount"]),
            checks,
            branches: n(&r["branches"]["totalCount"]),
            tags: n(&r["tags"]["totalCount"]),
            releases: n(&r["releases"]["totalCount"]),
            release: (!rel.is_null())
                .then(|| (str_of(rel, "name"), str_of(rel, "tagName"), iso_time(&str_of(rel, "publishedAt")), rel["isPrerelease"].as_bool().unwrap_or(false), str_of(rel, "url"))),
            prs: prs_v.map(|v| super::prs(&v, repo)).unwrap_or_default(),
            issues,
            runs: runs_v.map(|v| super::runs(&v)).unwrap_or_default(),
            views: traffic(views_v, "views"),
            clones: traffic(clones_v, "clones"),
        })
    })
}

/// The About of the shown revision asked for, unless it came or is coming.
fn want_about(app: &mut App, repo: &str) {
    let page = app.gh.pages.entry(repo.to_string()).or_default();
    let rev = page.rev();
    if page.about.as_ref().is_some_and(|(r, _)| *r == rev) || page.about_asked.as_ref() == Some(&rev) {
        return;
    }
    page.about_asked = Some(rev.clone());
    let (bin, root, tx) = (app.gh.bin.clone(), cwd(app), app.gh.tx.clone());
    let repo = repo.to_string();
    std::thread::spawn(move || {
        let a = load_about(&bin, &root, &repo, &rev).map(Box::new);
        let _ = tx.send(Msg::About(repo, rev, a));
    });
}

/// The About came (`tick`).
pub(super) fn about_came(app: &mut App, repo: String, rev: String, a: Result<Box<About>, String>) {
    let page = app.gh.pages.entry(repo).or_default();
    if page.about_asked.as_ref() == Some(&rev) {
        page.about_asked = None;
    }
    page.about = Some((rev, a.map(|a| *a)));
}

impl Page {
    /// The About when it is of the shown revision (its counts and checks are).
    fn about_now(&self) -> Option<&About> {
        self.about.as_ref().filter(|(r, _)| *r == self.rev()).and_then(|(_, a)| a.as_ref().ok())
    }
}

/// Where a README's picture comes from: a file of a repository at a revision ("HEAD":
/// its default branch), or one of GitHub's own image addresses.
#[derive(Clone, Debug, PartialEq)]
enum PicSource {
    File(String, String, String),
    Url(String),
}

/// `src` of a picture in a README in `folder` of `repo` at `rev`: a path in the repository
/// (beside the README, or from its root with "/"), a GitHub page or raw address of a
/// repository's file, or an image GitHub hosts. None: another site's, not fetched.
fn pic_source(src: &str, repo: &str, rev: &str, folder: &str) -> Option<PicSource> {
    let src = src.split(['?', '#']).next().unwrap_or(src);
    if src.is_empty() || src.starts_with("data:") {
        return None;
    }
    let Some((scheme, rest)) = src.split_once("://") else {
        let mut parts: Vec<&str> = if src.starts_with('/') { Vec::new() } else { folder.split('/').filter(|p| !p.is_empty()).collect() };
        for p in src.split('/') {
            match p {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        return Some(PicSource::File(repo.into(), rev.into(), parts.join("/")));
    };
    if scheme != "https" {
        return None;
    }
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let seg: Vec<&str> = path.split('/').collect();
    match host {
        // raw.githubusercontent.com/o/n/REF/path, github.com/o/n/(blob|raw)/REF/path.
        "raw.githubusercontent.com" if seg.len() >= 4 => Some(PicSource::File(format!("{}/{}", seg[0], seg[1]), seg[2].into(), seg[3..].join("/"))),
        "github.com" if seg.len() >= 5 && matches!(seg[2], "blob" | "raw") => Some(PicSource::File(format!("{}/{}", seg[0], seg[1]), seg[3].into(), seg[4..].join("/"))),
        "github.com" if seg.first() == Some(&"user-attachments") => Some(PicSource::Url(src.into())),
        h if h.ends_with(".githubusercontent.com") => Some(PicSource::Url(src.into())),
        _ => None,
    }
}

/// Where `s` is kept under `dir`.
fn pic_path(dir: &Path, s: &PicSource) -> PathBuf {
    let safe = |t: &str| t.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect::<String>();
    match s {
        PicSource::File(repo, rev, path) => {
            let mut p = dir.join(safe(repo)).join(safe(rev));
            for part in path.split('/').filter(|p| !p.is_empty() && *p != "..") {
                p.push(safe(part));
            }
            p
        }
        PicSource::Url(u) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            u.hash(&mut h);
            dir.join("web").join(format!("{:016x}", h.finish()))
        }
    }
}

/// A path for an address: its unreserved characters as they are, the rest %-escaped.
fn escaped(path: &str) -> String {
    path.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) { char::from(b).to_string() } else { format!("%{b:02X}") }).collect()
}

/// A picture's bytes through gh: a repository's file raw (its default branch for
/// "HEAD"), an address as it is; only a picture (by its first bytes), 20 MB at most.
fn fetch_pic(bin: &Path, root: &Path, s: &PicSource) -> Option<Vec<u8>> {
    let bytes = match s {
        PicSource::File(repo, rev, path) => {
            let at = if rev == "HEAD" { String::new() } else { format!("?ref={}", escaped(rev)) };
            gh(bin, root, &["api", "-H", "Accept: application/vnd.github.raw", &format!("repos/{repo}/contents/{}{at}", escaped(path))]).ok()?
        }
        PicSource::Url(u) => gh(bin, root, &["api", u]).ok()?,
    };
    (bytes.len() <= 20 << 20 && image::guess_format(&bytes).is_ok()).then_some(bytes)
}

/// The README's pictures not kept yet, fetched once each in the background.
fn want_pics(app: &mut App, repo: &str, wanted: Vec<PicSource>) {
    let dir = app.gh.pictures.clone();
    let page = app.gh.pages.entry(repo.to_string()).or_default();
    let new: Vec<(PicSource, PathBuf)> = wanted.into_iter().map(|s| (pic_path(&dir, &s), s)).filter(|(p, _)| page.pics_asked.insert(p.clone())).map(|(p, s)| (s, p)).collect();
    if new.is_empty() {
        return;
    }
    let (bin, root, tx, repo) = (app.gh.bin.clone(), cwd(app), app.gh.tx.clone(), repo.to_string());
    std::thread::spawn(move || {
        let _ = crate::private_dirs(&[dir]);
        for (s, path) in new {
            let Some(bytes) = fetch_pic(&bin, &root, &s) else { continue };
            let tmp = path.with_extension("part");
            if path.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok()) && std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
        let _ = tx.send(Msg::Pics(repo));
    });
}

/// Pictures came (`tick`): the README is drawn again with them.
pub(super) fn pics_came(app: &mut App, repo: &str) {
    if let Some(page) = app.gh.pages.get_mut(repo) {
        page.readme = None;
    }
}

fn want_stats(app: &mut App, repo: &str) {
    let page = app.gh.pages.entry(repo.to_string()).or_default();
    if page.stats.is_some() || std::mem::replace(&mut page.stats_asked, true) {
        return;
    }
    let (bin, root, tx) = (app.gh.bin.clone(), cwd(app), app.gh.tx.clone());
    let repo = repo.to_string();
    std::thread::spawn(move || {
        let st = load_stats(&bin, &root, &repo);
        let _ = tx.send(Msg::Stats(repo, st));
    });
}

/// The insights came (`tick`).
pub(super) fn stats_came(app: &mut App, repo: String, st: Result<Stats, String>) {
    let page = app.gh.pages.entry(repo).or_default();
    (page.stats, page.stats_asked) = (Some(st), false);
}

/// GitHub's colour for merged pull requests and closed issues.
const DONE: Color = Color::Rgb(137, 87, 229);

/// A bar `w` cells wide split by `parts` (count, colour); the empty rest dim.
fn bar(parts: &[(u64, Color)], w: usize) -> Vec<Span<'static>> {
    let total: u64 = parts.iter().map(|p| p.0).sum();
    let block = if crate::theme::ascii() { "=" } else { "━" };
    let mut out = Vec::new();
    let mut used = 0;
    for (k, &(n, c)) in parts.iter().enumerate() {
        let cells = if total == 0 {
            0
        } else if k + 1 == parts.len() {
            w - used
        } else {
            ((n as f64 / total as f64) * w as f64).round() as usize
        };
        let cells = cells.min(w - used);
        used += cells;
        out.push(Span::styled(block.repeat(cells), Style::new().fg(c)));
    }
    if used < w {
        out.push(Span::styled(block.repeat(w - used), Style::new().fg(mix(P.editor_bg, P.editor_fg, 0.12))));
    }
    out
}

/// Heat level 0..=4 of `n` against the largest, `max`.
fn level(n: u64, max: u64) -> u8 {
    if n == 0 || max == 0 {
        0
    } else {
        (n * 4).div_ceil(max).clamp(1, 4) as u8
    }
}

/// The main page's insights as lines, `w` wide.
fn insights(st: &Stats, w: usize) -> Vec<Line<'static>> {
    let dim = Style::new().fg(P.dim);
    let label = |t: &str| Span::styled(format!("  {t:<14}"), Style::new().fg(P.editor_fg).bold());
    let ic = icons();
    let sq = if crate::theme::ascii() { "#" } else { "■" };
    let mut v = Vec::new();
    v.push(Line::from(vec![
        Span::styled("  INSIGHTS  ".to_string(), Style::new().fg(P.sidebar_title).bold()),
        Span::styled(format!("{} {} stars · {} {} forks · {} watching", ic.gh[STAR], st.stars, ic.gh[FORK], st.forks, st.watchers), dim),
    ]));
    v.push(Line::raw(""));
    let room = w.saturating_sub(18 + 32).clamp(10, 40);
    let [open, merged, closed] = st.prs;
    let mut l = vec![label("Pull requests")];
    l.extend(bar(&[(open, P.gh_green), (merged, DONE), (closed, P.error_fg)], room));
    l.push(Span::styled(format!("  {open} open · {merged} merged · {closed} closed"), dim));
    v.push(Line::from(l));
    let [iopen, iclosed] = st.issues;
    let mut l = vec![label("Issues")];
    l.extend(bar(&[(iopen, P.gh_green), (iclosed, DONE)], room));
    l.push(Span::styled(format!("  {iopen} open · {iclosed} closed"), dim));
    v.push(Line::from(l));
    if !st.languages.is_empty() {
        let total: u64 = st.languages.iter().map(|l| l.1).sum::<u64>().max(1);
        let mut l = vec![label("Languages")];
        l.extend(bar(&st.languages.iter().map(|x| (x.1, x.2)).collect::<Vec<_>>(), room));
        v.push(Line::from(l));
        let mut legend = vec![Span::raw(" ".repeat(16))];
        for (name, size, c) in st.languages.iter().take(5) {
            legend.push(Span::styled(format!("{sq} "), Style::new().fg(*c)));
            legend.push(Span::styled(format!("{name} {:.0}%  ", *size as f64 * 100.0 / total as f64), dim));
        }
        v.push(Line::from(legend));
    }
    // The past year's commits, a column a week (as many weeks as fit), a row a weekday.
    if !st.weeks.is_empty() {
        let fit_weeks = (w.saturating_sub(18) / 2).min(st.weeks.len());
        let weeks = &st.weeks[st.weeks.len() - fit_weeks..];
        let max = weeks.iter().flatten().copied().max().unwrap_or(0);
        let total: u64 = st.weeks.iter().flatten().sum();
        v.push(Line::raw(""));
        v.push(Line::from(vec![label("Commits"), Span::styled(format!("{total} in the past year"), dim)]));
        for (d, name) in ["", "Mon", "", "Wed", "", "Fri", ""].iter().enumerate() {
            let mut l = vec![Span::styled(format!("  {name:<14}"), dim)];
            for wk in weeks {
                l.push(Span::styled(format!("{sq} "), Style::new().fg(level_color(level(wk[d], max)))));
            }
            v.push(Line::from(l));
        }
    }
    // When the commits land: a row a weekday, a column an hour.
    if st.punch.iter().flatten().any(|&n| n > 0) && w >= 18 + 48 {
        let max = st.punch.iter().flatten().copied().max().unwrap_or(0);
        v.push(Line::raw(""));
        v.push(Line::from(vec![label("When"), Span::styled("commits by weekday and hour (UTC)".to_string(), dim)]));
        let hours: String = (0..24).map(|h| if h % 3 == 0 { format!("{h:<2}") } else { "  ".into() }).collect();
        v.push(Line::styled(format!("  {:<14}{hours}", ""), dim));
        for (d, name) in ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].iter().enumerate() {
            let mut l = vec![Span::styled(format!("  {name:<14}"), dim)];
            for h in 0..24 {
                l.push(Span::styled(format!("{sq} "), Style::new().fg(level_color(level(st.punch[d][h], max)))));
            }
            v.push(Line::from(l));
        }
    }
    // Lines added and removed a week on the label's line: two sparklines (a column a
    // week, as many weeks as fit), each with its total.
    if st.churn.iter().any(|&(a, d)| a + d > 0) {
        let all = st.churn.iter().fold((0u64, 0u64), |(a, d), w| (a + w.0, d + w.1));
        let texts = format!(" +{} added", all.0).width() + format!(" -{} removed", all.1).width();
        let n = (w.saturating_sub(16 + texts + 3 + 1) / 2).min(st.churn.len());
        let weeks = &st.churn[st.churn.len() - n..];
        let max = weeks.iter().map(|&(a, d)| a.max(d)).max().unwrap_or(0).max(1);
        let ticks: Vec<char> = if crate::theme::ascii() { " .:-=+*#".chars().collect() } else { " ▁▂▃▄▅▆▇█".chars().collect() };
        let spark = |x: u64| ticks[((x as f64 / max as f64) * (ticks.len() - 1) as f64).round() as usize];
        let (added, removed): (u64, u64) = weeks.iter().fold((0, 0), |(a, d), w| (a + w.0, d + w.1));
        v.push(Line::raw(""));
        v.push(Line::from(vec![
            label("Code changes"),
            Span::styled(weeks.iter().map(|w| spark(w.0)).collect::<String>(), Style::new().fg(P.gh_green)),
            Span::styled(format!(" +{added} added   "), dim),
            Span::styled(weeks.iter().map(|w| spark(w.1)).collect::<String>(), Style::new().fg(P.error_fg)),
            Span::styled(format!(" -{removed} removed"), dim),
        ]));
    }
    if !st.people.is_empty() {
        let max = st.people.iter().map(|p| p.1).max().unwrap_or(1).max(1);
        v.push(Line::raw(""));
        v.push(Line::from(vec![label("Contributors")]));
        for (who, n) in &st.people {
            let cells = ((*n as f64 / max as f64) * room as f64).round().max(1.0) as usize;
            v.push(Line::from(vec![
                Span::styled(format!("  {:<14}", fit(who, 13)), Style::new().fg(P.editor_fg)),
                Span::styled(if crate::theme::ascii() { "=" } else { "━" }.repeat(cells), Style::new().fg(P.gh_green)),
                Span::styled(format!(" {n}"), dim),
            ]));
        }
    }
    v
}

/// A rule across the page (`w` columns), as the README's and the files' boundary.
fn rule(w: usize) -> Line<'static> {
    Line::styled(format!("  {}", (if crate::theme::ascii() { "-" } else { "─" }).repeat(w.saturating_sub(4))), Style::new().fg(P.group_border))
}

/// A chip: `text` on `bg`, in black or white, whichever reads.
fn chip(text: &str, bg: Color) -> Span<'static> {
    let fg = match bg {
        Color::Rgb(r, g, b) if u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 > 150_000 => Color::Rgb(0, 0, 0),
        _ => Color::Rgb(255, 255, 255),
    };
    Span::styled(format!(" {text} "), Style::new().fg(fg).bg(bg))
}

/// `text` in lines of at most `w` columns, cut at spaces.
fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match out.last_mut() {
            Some(l) if l.width() + 1 + word.width() <= w => {
                l.push(' ');
                l.push_str(word);
            }
            _ => out.push(fit(word, w)),
        }
    }
    out
}

/// One row of a list: its icon, its title (cut to fit), what it carries after the title
/// (chips), its meta on the right; a click opens `url`.
fn list_row(icon: (&str, Color), title: &str, chips: Vec<Span<'static>>, meta: &str, url: &str, w: usize) -> (Line<'static>, Option<Target>) {
    let used: usize = chips.iter().map(|c| c.content.width() + 1).sum();
    let room = w.saturating_sub(4 + icon.0.width() + used + meta.width() + 3);
    let title = fit(title, room);
    let pad = room.saturating_sub(title.width());
    let mut spans = vec![Span::raw("  "), Span::styled(icon.0.to_string(), Style::new().fg(icon.1)), Span::raw(" "), Span::styled(title, Style::new().fg(P.editor_fg))];
    for c in chips {
        spans.push(Span::raw(" "));
        spans.push(c);
    }
    spans.push(Span::raw(" ".repeat(pad + 2)));
    spans.push(Span::styled(format!("{meta} "), Style::new().fg(P.dim)));
    (Line::from(spans), (!url.is_empty()).then(|| Target::Url(url.to_string())))
}

/// A traffic chart: its rows of bars, a day's columns (a two-cell bar and a gap), the
/// y axis' columns before the bars; its lines: a title, the bars, the dates.
const CHART_ROWS: u16 = 5;
const DAY_W: u16 = 3;
const AXIS_W: usize = 7;
const CHART_LINES: usize = CHART_ROWS as usize + 2;

/// Where a traffic chart's bars are among the page's lines: the first bar row, the first
/// column, the days drawn (`chart_px` draws them again in pixels).
pub struct ChartAt {
    pub line: usize,
    pub x: u16,
    pub t: Traffic,
}

/// The unique part of a bar, solid; the rest of all, light.
fn chart_colors() -> (Color, Color) {
    (P.gh_green, crate::ui::pixels::opaque(P.gh_green).alpha(0.4).over(P.editor_bg))
}

/// A bar's height in eighths of a cell for `n` of `max` (one at least when there is any).
fn eighths(n: u64, max: u64) -> u16 {
    let full = u64::from(CHART_ROWS) * 8;
    if n == 0 { 0 } else { ((n * full + max / 2) / max.max(1)).clamp(1, full) as u16 }
}

/// Views or clones as GitHub's traffic graph: a title with its totals, a bar a day (its
/// unique part solid) over a y axis to the busiest day, the first and last dates under
/// it. CHART_LINES lines, each `width` columns.
fn chart(name: &str, t: &Traffic) -> (Vec<Vec<Span<'static>>>, usize) {
    let (solid, light) = chart_colors();
    let dim = Style::new().fg(P.dim);
    let sq = if crate::theme::ascii() { "#" } else { "■" };
    let width = 2 + AXIS_W + t.days.len() * DAY_W as usize;
    let mut out = vec![vec![
        Span::styled(format!("  {name}  "), Style::new().fg(P.editor_fg).bold()),
        Span::styled(format!("{sq} "), Style::new().fg(light)),
        Span::styled(format!("{} total  ", t.all), dim),
        Span::styled(format!("{sq} "), Style::new().fg(solid)),
        Span::styled(format!("{} unique", t.unique), dim),
    ]];
    let max = t.days.iter().map(|d| d.1).max().unwrap_or(0);
    let blocks: Vec<char> = if crate::theme::ascii() { " .:-=+*##".chars().collect() } else { " ▁▂▃▄▅▆▇█".chars().collect() };
    for row in (0..CHART_ROWS).rev() {
        let tick = match row {
            r if r == CHART_ROWS - 1 => max.to_string(),
            0 => "0".into(),
            _ => String::new(),
        };
        let mut l = vec![Span::styled(format!("  {tick:>w$} ", w = AXIS_W - 1), dim)];
        for (_, all, unique) in &t.days {
            let lo = row * 8;
            let (a, u) = (eighths(*all, max).saturating_sub(lo).min(8), eighths(*unique, max).saturating_sub(lo).min(8));
            let c = if u > 0 && u * 2 >= a { solid } else { light };
            let g = blocks[a as usize];
            l.push(Span::styled(format!("{g}{g} "), Style::new().fg(c)));
        }
        out.push(l);
    }
    // The first and last days' dates, under their bars.
    let mut axis = format!("  {:w$}", "", w = AXIS_W);
    if let (Some(first), Some(last)) = (t.days.first(), t.days.last()) {
        let span = t.days.len() * DAY_W as usize;
        let gap = span.saturating_sub(first.0.width() + last.0.width() + 1);
        axis.push_str(&format!("{}{}{}", first.0, " ".repeat(gap), if t.days.len() > 1 { last.0.as_str() } else { "" }));
    }
    out.push(vec![Span::styled(axis, dim)]);
    (out, width)
}

/// The bars of `t` in pixels, `cell` (w, h) a cell: as `chart` draws them, to the pixel,
/// their tops rounded, over a faint line at the busiest day's height and the baseline.
fn chart_px(t: &Traffic, cell: (u16, u16)) -> image::RgbaImage {
    let (cw, ch) = (u32::from(cell.0), u32::from(cell.1));
    let (w, h) = (t.days.len() as u32 * u32::from(DAY_W) * cw, u32::from(CHART_ROWS) * ch);
    let mut img = image::RgbaImage::new(w.max(1), h.max(1));
    let px = |c: Color| {
        let crate::theme::Rgba(r, g, b, a) = crate::ui::pixels::opaque(c);
        image::Rgba([r, g, b, a])
    };
    let (solid, light) = chart_colors();
    let (solid, light, grid) = (px(solid), px(light), px(P.guide));
    for x in 0..w {
        img.put_pixel(x, 0, grid);
        img.put_pixel(x, h - 1, grid);
    }
    let max = t.days.iter().map(|d| d.1).max().unwrap_or(0).max(1);
    let tall = |n: u64| if n == 0 { 0 } else { ((n * u64::from(h) + max / 2) / max).clamp(2, u64::from(h)) as u32 };
    for (i, (_, all, unique)) in t.days.iter().enumerate() {
        let x0 = i as u32 * u32::from(DAY_W) * cw + 1;
        let bw = 2 * cw - 2;
        let (ha, hu) = (tall(*all), tall(*unique).min(tall(*all)));
        for y in h - ha..h {
            for x in x0..x0 + bw {
                // The top corners cut: a rounded bar.
                let corner = y == h - ha && (x == x0 || x == x0 + bw - 1);
                if !corner {
                    img.put_pixel(x, y, if y >= h - hu { solid } else { light });
                }
            }
        }
    }
    img
}

/// The main page's ABOUT (with the latest release and the traffic), then its open pull
/// requests and issues and its latest runs: each row of a list opens it in the browser.
fn about_lines(a: &About, open: (u64, u64), w: usize, link: Color, now: i64) -> (Vec<(Line<'static>, Option<Target>)>, Vec<ChartAt>) {
    let dim = Style::new().fg(P.dim);
    let title = |t: &str| Span::styled(format!("  {t}  "), Style::new().fg(P.sidebar_title).bold());
    let label = |t: &str| Span::styled(format!("  {t:<14}"), Style::new().fg(P.editor_fg).bold());
    let ic = icons();
    let mut v: Vec<(Line<'static>, Option<Target>)> = Vec::new();
    let mut head = vec![title("ABOUT")];
    if !a.visibility.is_empty() {
        let vis = a.visibility[..1].to_string() + &a.visibility[1..].to_lowercase();
        head.push(chip(&vis, if a.visibility == "PUBLIC" { P.gh_green } else { P.gh_yellow }));
    }
    if a.archived {
        head.push(Span::raw(" "));
        head.push(chip("Archived", P.error_fg));
    }
    if !a.parent.is_empty() {
        head.push(Span::styled(format!("  {} forked from {}", ic.gh[FORK], a.parent), dim));
    }
    v.push((Line::from(head), None));
    if a.description.is_empty() {
        v.push((Line::styled("  No description.", dim), None));
    }
    for l in wrap(&a.description, w.saturating_sub(4)) {
        v.push((Line::styled(format!("  {l}"), Style::new().fg(P.editor_fg)), None));
    }
    if !a.homepage.is_empty() {
        v.push((Line::from(vec![Span::styled(format!("  {} ", ic.gh[OPEN]), dim), Span::styled(fit(&a.homepage, w.saturating_sub(6)), Style::new().fg(link).underlined())]), Some(Target::Url(a.homepage.clone()))));
    }
    // The topics as GitHub's pills, as many rows as they take.
    let mut row: Vec<Span<'static>> = vec![Span::raw("  ")];
    let mut used = 2;
    for t in &a.topics {
        if used + t.width() + 3 > w && used > 2 {
            v.push((Line::from(std::mem::replace(&mut row, vec![Span::raw("  ")])), None));
            used = 2;
        }
        row.push(Span::styled(format!(" {t} "), Style::new().fg(link).bg(P.widget_bg)));
        row.push(Span::raw(" "));
        used += t.width() + 3;
    }
    if row.len() > 1 {
        v.push((Line::from(row), None));
    }
    let mut facts = Vec::new();
    if !a.license.is_empty() {
        facts.push(format!("{} license", a.license));
    }
    facts.push(crate::size::human(a.size_kb * 1024));
    facts.push(format!("created {}", crate::scm::ago(a.created, now)));
    facts.push(format!("pushed {}", crate::scm::ago(a.pushed, now)));
    v.push((Line::styled(format!("  {}", facts.join(" · ")), dim), None));
    // The latest release.
    v.push((Line::raw(""), None));
    match &a.release {
        Some((name, tag, t, pre, url)) => {
            let shown = if name.is_empty() || name == tag { tag.clone() } else { format!("{name} ({tag})") };
            let mut l = vec![label("Releases"), Span::styled(format!("{} ", a.releases), dim), Span::styled(shown, Style::new().fg(P.editor_fg).bold()), Span::raw(" ")];
            l.push(if *pre { chip("Pre-release", P.gh_yellow) } else { chip("Latest", P.gh_green) });
            l.push(Span::styled(format!("  {}", crate::scm::ago(*t, now)), dim));
            v.push((Line::from(l), (!url.is_empty()).then(|| Target::Url(url.clone()))));
        }
        None => v.push((Line::from(vec![label("Releases"), Span::styled("none published", dim)]), None)),
    }
    // Views and clones over two weeks, where the account may see them: side by side
    // when they fit, else one under the other.
    let mut charts = Vec::new();
    let shown: Vec<(&str, &Traffic)> = [("Views", &a.views), ("Clones", &a.clones)].into_iter().filter_map(|(n, t)| Some((n, t.as_ref()?))).collect();
    if !shown.is_empty() {
        v.push((Line::raw(""), None));
        v.push((Line::from(vec![title("TRAFFIC"), Span::styled("past 14 days".to_string(), dim)]), None));
        let built: Vec<_> = shown.iter().map(|(n, t)| chart(n, t)).collect();
        let together = built.iter().map(|b| b.1 + 4).sum::<usize>() <= w;
        let groups: Vec<Vec<usize>> = if together { vec![(0..built.len()).collect()] } else { (0..built.len()).map(|i| vec![i]).collect() };
        for group in groups {
            let first = v.len();
            let mut x = 0usize;
            let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new(); CHART_LINES];
            for &i in &group {
                let (lines, width) = &built[i];
                for (row, l) in rows.iter_mut().zip(lines) {
                    let used: usize = row.iter().map(|s| s.content.width()).sum();
                    row.push(Span::raw(" ".repeat(x.saturating_sub(used))));
                    row.extend(l.iter().cloned());
                }
                charts.push(ChartAt { line: first + 1, x: (x + 2 + AXIS_W) as u16, t: shown[i].1.clone() });
                x += width + 4;
            }
            v.extend(rows.into_iter().map(|r| (Line::from(r), None)));
        }
    }
    // The open pull requests and issues, the latest runs.
    let green = P.gh_green;
    v.push((Line::raw(""), None));
    v.push((Line::from(vec![title("PULL REQUESTS"), Span::styled(format!("{} open", open.0.max(a.prs.len() as u64)), dim)]), None));
    if a.prs.is_empty() {
        v.push((Line::styled("  None open.", dim), None));
    }
    for p in &a.prs {
        let icon = if p.draft { (ic.gh[PR_DRAFT], P.dim) } else { (ic.gh[PR], green) };
        let check = match p.checks {
            Checks::Pass => vec![Span::styled(ic.gh[PASS].to_string(), Style::new().fg(green))],
            Checks::Fail => vec![Span::styled(ic.gh[FAIL].to_string(), Style::new().fg(P.error_fg))],
            Checks::Pending => vec![Span::styled(ic.gh[RUNNING].to_string(), Style::new().fg(P.gh_yellow))],
            Checks::None => Vec::new(),
        };
        let mut chips = check;
        if p.draft {
            chips.push(chip("Draft", P.dim));
        }
        v.push(list_row(icon, &format!("#{} {}", p.number, p.title), chips, &format!("{} · {}", p.author, crate::scm::ago_short(p.updated, now)), &p.url, w));
    }
    v.push((Line::raw(""), None));
    v.push((Line::from(vec![title("ISSUES"), Span::styled(format!("{} open", open.1.max(a.issues.len() as u64)), dim)]), None));
    if a.issues.is_empty() {
        v.push((Line::styled("  None open.", dim), None));
    }
    for (i, labels) in &a.issues {
        let chips = labels.iter().take(3).map(|(n, c)| chip(n, *c)).collect();
        v.push(list_row((ic.gh[ISSUE], green), &format!("#{} {}", i.number, i.title), chips, &format!("{} · {}", i.author, crate::scm::ago_short(i.updated, now)), &i.url, w));
    }
    v.push((Line::raw(""), None));
    v.push((Line::from(vec![title("ACTIONS"), Span::styled("latest runs".to_string(), dim)]), None));
    if a.runs.is_empty() {
        v.push((Line::styled("  No runs.", dim), None));
    }
    for r in &a.runs {
        let meta = format!("{} · {}", r.branch, crate::scm::ago_short(r.updated, now));
        v.push(list_row(super::draw::run_icon(r.state), &format!("{} · {}", r.workflow, r.title), Vec::new(), &meta, &r.url, w));
    }
    (v, charts)
}

/// The repository's page in the active group (a preview tab unless `pin`), on its main page.
pub fn open(app: &mut App, repo: &str, pin: bool) {
    app.show_tab(app.ag, tab_key(repo), !pin);
    want_dir(app, repo, "");
}

/// Folder `path` asked for, unless it came or is coming.
fn want_dir(app: &mut App, repo: &str, path: &str) {
    let page = app.gh.pages.entry(repo.to_string()).or_default();
    if page.dirs.contains_key(path) || !page.asked.insert(path.to_string()) {
        return;
    }
    let rev = page.rev();
    let (bin, root, tx) = (app.gh.bin.clone(), cwd(app), app.gh.tx.clone());
    let (repo, path) = (repo.to_string(), path.to_string());
    std::thread::spawn(move || {
        let d = load_dir(&bin, &root, &repo, &rev, &path);
        let _ = tx.send(Msg::Dir(repo, rev, path, d));
    });
}

/// A folder came (`tick`): kept unless the branch changed meanwhile.
pub(super) fn dir_came(app: &mut App, repo: String, rev: String, path: String, d: Result<Dir, String>) {
    let page = app.gh.pages.entry(repo).or_default();
    if page.rev() == rev {
        page.asked.remove(&path);
        page.dirs.insert(path, d);
    }
}

/// File `path` in the middle (fetched first when it has not been).
fn show_file(app: &mut App, repo: &str, path: &str) {
    let page = app.gh.pages.entry(repo.to_string()).or_default();
    page.file = Some(path.to_string());
    if page.files.contains_key(path) {
        return install(app, repo);
    }
    app.docs.remove(&tab_key(repo)); // "Loading…" until it comes
    if !page.files_asked.insert(path.to_string()) {
        return;
    }
    let rev = page.rev();
    let (bin, root, tx) = (app.gh.bin.clone(), cwd(app), app.gh.tx.clone());
    let (repo, path) = (repo.to_string(), path.to_string());
    std::thread::spawn(move || {
        let text = load_blob(&bin, &root, &repo, &rev, &path).map(|t| t.lines().map(str::to_string).collect());
        let _ = tx.send(Msg::Blob(repo, rev, path, text));
    });
}

/// A file came (`tick`): shown when it is still the one asked for, on the same branch.
pub(super) fn blob_came(app: &mut App, repo: String, rev: String, path: String, text: Result<Vec<String>, String>) {
    let page = app.gh.pages.entry(repo.clone()).or_default();
    if page.rev() != rev {
        return;
    }
    page.files_asked.remove(&path);
    page.files.insert(path.clone(), text);
    if page.file.as_deref() == Some(path.as_str()) {
        install(app, &repo);
    }
}

/// The shown file's text as the page tab's document (read-only), from its top.
fn install(app: &mut App, repo: &str) {
    let Some(page) = app.gh.pages.get(repo) else { return };
    let Some(path) = page.file.clone() else { return };
    let lines = match page.files.get(&path) {
        Some(Ok(l)) => l.clone(),
        Some(Err(e)) => vec![e.clone()],
        None => return,
    };
    let why = format!("{repo} on GitHub, {}: read-only", page.branch_name());
    let key = tab_key(repo);
    app.docs.insert(key.clone(), Doc::Text(Buffer::from_lines(&path, lines, &why)));
    reset_views(app, &key);
}

fn reset_views(app: &mut App, key: &str) {
    for t in app.groups.iter_mut().flat_map(|g| g.tabs.iter_mut()).filter(|t| t.key == key) {
        t.view = View::default();
    }
}

/// The main page in the middle, listing `folder` (opened in the tree too).
fn show_main(app: &mut App, repo: &str, folder: &str) {
    let page = app.gh.pages.entry(repo.to_string()).or_default();
    (page.file, page.folder) = (None, folder.to_string());
    let mut at = String::new();
    for part in folder.split('/').filter(|p| !p.is_empty()) {
        at = if at.is_empty() { part.to_string() } else { format!("{at}/{part}") };
        page.open.insert(at.clone());
    }
    let key = tab_key(repo);
    app.docs.remove(&key);
    reset_views(app, &key);
    want_dir(app, repo, folder);
}

/// The header's branch: its branches in a quick pick (fetched each time: they change).
fn pick_branch(app: &mut App, repo: &str) {
    let page = app.gh.pages.entry(repo.to_string()).or_default();
    if std::mem::replace(&mut page.branches_asked, true) {
        return;
    }
    app.msg = "listing the branches…".into();
    let (bin, root, tx) = (app.gh.bin.clone(), cwd(app), app.gh.tx.clone());
    let repo = repo.to_string();
    std::thread::spawn(move || {
        let b = load_branches(&bin, &root, &repo);
        let _ = tx.send(Msg::Branches(repo, b));
    });
}

/// The branches came (`tick`): picked from in a quick pick, the shown one first.
pub(super) fn branches_came(app: &mut App, repo: String, b: Result<Vec<String>, String>) {
    let Some(page) = app.gh.pages.get_mut(&repo) else { return };
    page.branches_asked = false;
    let names = match b {
        Ok(n) => n,
        Err(e) => {
            app.msg = format!("GitHub: {e}");
            return;
        }
    };
    let current = page.branch_name();
    let default = page.dirs.get("").and_then(|d| d.as_ref().ok()).map(|d| d.default_branch.clone()).unwrap_or_default();
    app.msg.clear();
    let mut items: Vec<crate::gitcmds::Item> = names
        .into_iter()
        .map(|n| {
            let detail = match (n == current, n == default) {
                (true, true) => "shown · default",
                (true, false) => "shown",
                (false, true) => "default",
                _ => "",
            };
            crate::gitcmds::Item { label: n.clone(), detail: detail.into(), pick: crate::gitcmds::Pick::GitHubBranch(repo.clone(), n) }
        })
        .collect();
    items.sort_by_key(|i| i.label != current);
    crate::gitcmds::open_picker(app, &format!("Switch the branch of {repo}"), items, None);
}

/// Branch `name` shown: everything fetched again from it, its main page first; the folders
/// open in the tree stay open.
pub fn switch_branch(app: &mut App, repo: &str, name: &str) {
    let Some(page) = app.gh.pages.get_mut(repo) else { return };
    let default = page.dirs.get("").and_then(|d| d.as_ref().ok()).map(|d| d.default_branch.clone());
    let open = std::mem::take(&mut page.open);
    *page = Page { branch: (default.as_deref() != Some(name)).then(|| name.to_string()), open: open.clone(), ..Page::default() };
    show_main(app, repo, "");
    for path in open {
        want_dir(app, repo, &path);
    }
    app.msg = format!("{repo}: {name}");
}

/// A row of the tree: its depth, then an entry (name, path, a folder, open) or a note.
enum TreeRow {
    Entry(String, String, bool, bool),
    Note(String),
}

fn tree(page: &Page) -> Vec<(usize, TreeRow)> {
    fn walk(page: &Page, path: &str, depth: usize, out: &mut Vec<(usize, TreeRow)>) {
        match page.dirs.get(path) {
            None => out.push((depth, TreeRow::Note("Loading…".into()))),
            Some(Err(e)) => out.push((depth, TreeRow::Note(e.clone()))),
            Some(Ok(d)) => {
                for e in &d.entries {
                    let open = e.dir && page.open.contains(&e.path);
                    out.push((depth, TreeRow::Entry(e.name.clone(), e.path.clone(), e.dir, open)));
                    if open {
                        walk(page, &e.path, depth + 1, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(page, "", 0, &mut out);
    out
}

/// The active group's repository page, when its tab is one.
fn active_repo(app: &App) -> Option<String> {
    let g = app.groups.get(app.ag)?;
    g.tabs.get(g.active)?.key.strip_prefix("repo:").map(str::to_string)
}

/// A click on the page's button, header, list, tree or outline. True when it was the page's.
pub fn click(app: &mut App, p: Position, _double: bool) -> bool {
    let Some(t) = app.gh.hits.page.iter().find(|(r, _)| r.contains(p)).map(|(_, t)| t.clone()) else { return false };
    let Some(repo) = active_repo(app) else { return false };
    match t {
        Target::Folder(path) => {
            let page = app.gh.pages.entry(repo.clone()).or_default();
            if !page.open.remove(&path) {
                page.open.insert(path.clone());
                want_dir(app, &repo, &path);
            }
        }
        Target::Into(path) => show_main(app, &repo, &path),
        Target::File(path) => show_file(app, &repo, &path),
        Target::Main => show_main(app, &repo, ""),
        Target::Branches => pick_branch(app, &repo),
        Target::Url(u) => super::open_url(app, &u),
        Target::Symbol(line) => {
            if let Some(t) = app.active_tab() {
                (t.view.cursor, t.view.anchor) = (Pos::new(line, 0), None);
                t.view.scroll = line.saturating_sub(3);
            }
            app.reveal();
        }
    }
    app.focus = Focus::Editor;
    true
}

/// The wheel over the page's tree scrolls it.
pub fn wheel(app: &mut App, p: Position, d: isize) -> bool {
    if !app.gh.hits.page_tree.contains(p) {
        return false;
    }
    let Some(page) = active_repo(app).and_then(|r| app.gh.pages.get_mut(&r)) else { return false };
    page.tree_off = page.tree_off.saturating_add_signed(d); // the frame clamps it
    true
}

/// The page's frame in `body`: the header over the middle (the repository, the path, the
/// branch; then the last commit) and the right pane (the main page button, the tree, the
/// file's outline under it). Returns the middle, for the main page or the file's code.
pub fn draw_frame(app: &mut App, f: &mut Frame, g: usize, repo: &str, body: Rect) -> Rect {
    fill(f, body, P.editor_bg);
    want_dir(app, repo, ""); // a page restored with the session
    let fi = app.settings.file_icons.clone();
    let hover = app.hover;
    let key = tab_key(repo);
    // The outline: the shown file's symbols, nested by its folds. (The buffer has no path
    // of its own: the language comes from the file's.)
    let outline: Vec<(usize, crate::symbols::Symbol)> = {
        let grp = &app.groups[g];
        let folds = grp.tabs.get(grp.active).map(|t| t.view.fold_ends.clone()).unwrap_or_default();
        let file = app.gh.pages.get(repo).and_then(|p| p.file.clone()).unwrap_or_default();
        match app.docs.get(&key) {
            Some(Doc::Text(b)) => {
                let syms = crate::symbols::symbols(&file, &b.lines);
                let depths = crate::symbols::depths(&syms, &b.lines, &folds);
                depths.into_iter().zip(syms).collect()
            }
            _ => Vec::new(),
        }
    };
    let Some(page) = app.gh.pages.get_mut(repo) else { return body };
    let side_w = if body.width >= 60 { (body.width / 3).clamp(SIDE_MIN, SIDE_MAX) } else { 0 };
    let mid = Rect::new(body.x, body.y, body.width - side_w, body.height);
    let dim = Style::new().fg(P.dim);
    let mut hits: Vec<(Rect, Target)> = Vec::new();
    // The header: the repository (the main page), the path, the branch (to switch it).
    let mut head = vec![Span::styled(format!(" {} ", icons().gh[REPO]), Style::new().fg(P.icon_fg))];
    let name = Span::styled(repo.to_string(), Style::new().fg(P.focus_border).bold());
    hits.push((Rect::new(mid.x + 3, mid.y, name.width() as u16, 1), Target::Main));
    head.push(name);
    let path = page.file.clone().unwrap_or_else(|| page.folder.clone());
    for part in path.split('/').filter(|s| !s.is_empty()) {
        head.push(Span::styled(" / ", dim));
        head.push(Span::styled(part.to_string(), Style::new().fg(P.editor_fg).bold()));
    }
    let branch = page.branch_name();
    if !branch.is_empty() {
        let br = format!(" {} {} {} ", icons().gh[GITHUB], branch, icons().chev_down);
        let used: usize = head.iter().map(|s| s.content.width()).sum();
        let gap = (mid.width as usize).saturating_sub(used + br.width() + 1);
        let bx = mid.x + (used + gap) as u16;
        let r = Rect::new(bx, mid.y, (br.width() as u16).min(mid.right().saturating_sub(bx)), 1);
        head.push(Span::raw(" ".repeat(gap)));
        head.push(Span::styled(br, crate::ui::kit::look_at(crate::ui::kit::Look::Secondary, r)));
        hits.push((r, Target::Branches));
    }
    f.render_widget(Line::from(head), Rect::new(mid.x, mid.y, mid.width, 1));
    // Then the file's last commit, or (the main page) the branch's latest.
    let now = crate::scm::now();
    let last = match &page.file {
        Some(p) => {
            let parent = p.rsplit_once('/').map_or("", |(d, _)| d);
            let e = page.dirs.get(parent).and_then(|d| d.as_ref().ok()).and_then(|d| d.entries.iter().find(|e| &e.path == p));
            e.and_then(|e| e.last.clone()).map(|(m, t)| (String::new(), m, String::new(), t))
        }
        None => page.dirs.get("").and_then(|d| d.as_ref().ok()).and_then(|d| d.head.clone()),
    };
    // The branch's checks before its latest commit's hash, as GitHub's tick or cross.
    let check = match page.about_now().filter(|_| page.file.is_none()).map(|a| a.checks) {
        Some(Checks::Pass) => Some((icons().gh[PASS], P.gh_green)),
        Some(Checks::Fail) => Some((icons().gh[FAIL], P.error_fg)),
        Some(Checks::Pending) => Some((icons().gh[RUNNING], P.gh_yellow)),
        _ => None,
    };
    if let Some((who, msg, hash, t)) = last.filter(|_| mid.height > 2) {
        let right = if hash.is_empty() { format!("{} ", crate::scm::ago(t, now)) } else { format!("{hash} · {} ", crate::scm::ago(t, now)) };
        let left = if who.is_empty() { "  ".to_string() } else { format!("  {who}  ") };
        let mark = check.map_or(Span::raw(""), |(g, c)| Span::styled(format!("{g} "), Style::new().fg(c)));
        let room = (mid.width as usize).saturating_sub(left.width() + mark.width() + right.width() + 1);
        let r = Rect::new(mid.x, mid.y + 1, mid.width, 1);
        fill(f, r, P.widget_bg);
        f.render_widget(
            Line::from(vec![
                Span::styled(left, Style::new().fg(P.widget_fg).bold()),
                Span::styled(format!("{:<room$}", fit(&msg, room)), Style::new().fg(P.widget_fg)),
                Span::raw(" "),
                mark,
                Span::styled(right, dim),
            ]),
            r,
        );
    }
    // The right pane: the main page button, FILES, and OUTLINE under it while a file with
    // symbols shows.
    if side_w > 0 {
        let side = Rect::new(mid.right(), body.y, side_w, body.height);
        fill(f, side, P.sidebar_bg);
        for y in side.y..side.bottom() {
            f.buffer_mut()[(side.x, y)].set_symbol("│").set_fg(P.guide);
        }
        let inner = Rect::new(side.x + 1, side.y, side.width - 1, side.height);
        let title = |f: &mut Frame, y: u16, t: &str| f.render_widget(Span::styled(format!(" {t}"), Style::new().fg(P.sidebar_title).bold()), Rect::new(inner.x, y, inner.width, 1));
        // The button: the main page; lit while it shows.
        let label = format!(" {} {} ", icons().gh[REPO], fit(repo, (inner.width as usize).saturating_sub(6)));
        let br = Rect::new(inner.x + 1, inner.y, (label.width() as u16).min(inner.width.saturating_sub(2)), 1);
        let on_main = page.file.is_none() && page.folder.is_empty();
        crate::ui::kit::button(f, br, label.trim(), if on_main { crate::ui::kit::Look::Primary } else { crate::ui::kit::Look::Secondary }, P.editor_bg, false);
        hits.push((br, Target::Main));
        let out_h = if outline.is_empty() { 0 } else { (outline.len() as u16 + 1).min(inner.height * 2 / 5).max(3) };
        let tree_area = Rect::new(inner.x, inner.y + 3, inner.width, inner.height.saturating_sub(3 + out_h));
        title(f, inner.y + 2, "FILES");
        let rows = tree(page);
        page.tree_off = page.tree_off.min(rows.len().saturating_sub(tree_area.height as usize));
        for (k, (depth, row)) in rows.iter().skip(page.tree_off).take(tree_area.height as usize).enumerate() {
            let r = Rect::new(tree_area.x, tree_area.y + k as u16, tree_area.width, 1);
            let lead = " ".repeat(1 + 2 * depth);
            match row {
                TreeRow::Note(n) => f.render_widget(Span::styled(fit(&format!("{lead}  {n}"), r.width as usize), dim), r),
                TreeRow::Entry(name, path, dir, open) => {
                    let shown = page.file.as_deref() == Some(path.as_str()) || page.file.is_none() && *dir && page.folder == *path;
                    let bg = if shown { Some(P.list_active_bg) } else { hover.filter(|p| r.contains(*p)).map(|_| P.list_hover_bg) };
                    if let Some(bg) = bg {
                        fill(f, r, bg);
                    }
                    let (chev, (glyph, gfg)) = if *dir {
                        (if *open { icons().chev_down } else { icons().chev_right }, folder_icon(name, *open, &fi))
                    } else {
                        (" ", file_icon(name, &fi))
                    };
                    let used = lead.width() + 4;
                    f.render_widget(
                        Line::from(vec![
                            Span::raw(lead.clone()),
                            Span::styled(format!("{chev} "), Style::new().fg(P.icon_fg)),
                            Span::styled(glyph, Style::new().fg(gfg)),
                            Span::styled(fit(&format!(" {name}"), (r.width as usize).saturating_sub(used)), Style::new().fg(P.sidebar_fg)),
                        ]),
                        r,
                    );
                    hits.push((r, if *dir { Target::Folder(path.clone()) } else { Target::File(path.clone()) }));
                }
            }
        }
        app.gh.hits.page_tree = tree_area;
        if out_h > 0 {
            let oy = inner.bottom() - out_h;
            for x in inner.x..inner.right() {
                f.buffer_mut()[(x, oy)].set_symbol("─").set_fg(P.guide);
            }
            title(f, oy, "OUTLINE ");
            let ic = icons();
            for (k, (depth, s)) in outline.iter().take(out_h as usize - 1).enumerate() {
                let r = Rect::new(inner.x, oy + 1 + k as u16, inner.width, 1);
                if hover.is_some_and(|p| r.contains(p)) {
                    fill(f, r, P.list_hover_bg);
                }
                let kind = crate::symbols::kind_index(s.kind);
                let lead = " ".repeat(1 + 2 * depth);
                let used = lead.width() + 2;
                f.render_widget(
                    Line::from(vec![
                        Span::raw(lead),
                        Span::styled(ic.symbols[kind], Style::new().fg(P.symbol[kind])),
                        Span::styled(fit(&format!(" {}", s.name), (r.width as usize).saturating_sub(used)), Style::new().fg(P.sidebar_fg)),
                    ]),
                    r,
                );
                hits.push((r, Target::Symbol(s.line)));
            }
        }
    }
    app.gh.hits.page.extend(hits);
    let top = if mid.height > 2 { 2 } else { 1 };
    Rect::new(mid.x, mid.y + top, mid.width, mid.height.saturating_sub(top))
}

/// The main page in `area`, scrolled by the tab's view: the folder's files and folders (`..`
/// first below the root) each with its last commit, then its README. Returns its rows.
pub fn draw_main(app: &mut App, f: &mut Frame, g: usize, repo: &str, area: Rect) -> usize {
    let link = {
        let empty = serde_json::Map::new();
        let colors = app.settings.theme.json["colors"].as_object().unwrap_or(&empty);
        crate::theme::lookup(colors, "textLink.foreground").map_or(P.focus_border, |c| c.over(P.editor_bg))
    };
    let fi = app.settings.file_icons.clone();
    let hover = app.hover;
    let folder = app.gh.pages.get(repo).map(|p| p.folder.clone()).unwrap_or_default();
    // A cell's size in pixels sizes the README's pictures (as the Markdown preview's).
    let cell = crate::media::cell(app);
    let pictures = app.gh.pictures.clone();
    let mut wanted: Vec<PicSource> = Vec::new();
    let mut readme_pics: Option<(usize, Vec<crate::mdpreview::Pic>)> = None;
    want_dir(app, repo, &folder);
    if folder.is_empty() {
        want_stats(app, repo);
        want_about(app, repo);
    }
    let Some(page) = app.gh.pages.get_mut(repo) else { return 0 };
    let dim = Style::new().fg(P.dim);
    let w = area.width as usize;
    let now = crate::scm::now();
    // The root's counts over its files, as GitHub's: its branches pick one.
    let counts = page.about_now().filter(|_| page.folder.is_empty() && page.file.is_none()).map(|a| {
        // Codicons' git-commit and tag; plain marks without them.
        let (commit, tag) = if crate::theme::ascii() { ("o", "#") } else { ("\u{eafc}", "\u{ea66}") };
        let fg = Style::new().fg(P.editor_fg);
        Line::from(vec![
            Span::styled(format!("  {commit} {} commits", a.commits), fg),
            Span::styled(format!("   {} {} branches", icons().branch, a.branches), fg),
            Span::styled(format!("   {tag} {} tags", a.tags), fg),
        ])
    });
    let mut charts: Vec<ChartAt> = Vec::new();
    let mut rules: Vec<usize> = Vec::new();
    let pick = counts.is_some().then_some(Target::Branches);
    let mut lines: Vec<(Line<'static>, Option<Target>)> = vec![(counts.unwrap_or_default(), pick)];
    match (&page.file, page.dirs.get(&page.folder)) {
        (Some(p), _) => lines.push((Line::styled(format!("  Loading {p}…"), dim), None)),
        (None, None) => lines.push((Line::styled("  Loading…", dim), None)),
        (None, Some(Err(e))) => lines.push((Line::styled(format!("  {e}"), Style::new().fg(P.error_fg)), None)),
        (None, Some(Ok(d))) => {
            let name_w = d.entries.iter().map(|e| e.name.width()).max().unwrap_or(0).clamp(8, 40);
            if !page.folder.is_empty() {
                let (glyph, gfg) = folder_icon("..", true, &fi);
                let up = page.folder.rsplit_once('/').map_or("", |(d, _)| d).to_string();
                lines.push((Line::from(vec![Span::raw("  "), Span::styled(glyph, Style::new().fg(gfg)), Span::styled(" ..", Style::new().fg(P.editor_fg))]), Some(Target::Into(up))));
            }
            for e in &d.entries {
                let (glyph, gfg) = if e.dir { folder_icon(&e.name, false, &fi) } else { file_icon(&e.name, &fi) };
                let (msg, when) = e.last.as_ref().map_or((String::new(), String::new()), |(m, t)| (m.clone(), crate::scm::ago(*t, now)));
                let room = w.saturating_sub(3 + name_w + 2 + when.width() + 2);
                lines.push((
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(glyph, Style::new().fg(gfg)),
                        Span::raw(" "),
                        Span::styled(format!("{:<name_w$}", fit(&e.name, name_w)), Style::new().fg(if e.dir { P.editor_fg } else { P.sidebar_fg })),
                        Span::raw("  "),
                        Span::styled(format!("{:<room$}", fit(&msg, room)), dim),
                        Span::styled(format!("{when} "), dim),
                    ]),
                    Some(if e.dir { Target::Into(e.path.clone()) } else { Target::File(e.path.clone()) }),
                ));
            }
            if d.entries.is_empty() {
                lines.push((Line::styled("  (an empty folder)", dim), None));
            }
            // The root's About and lists, then its insights, under its files: a rule
            // between them.
            if page.folder.is_empty() {
                lines.push((Line::raw(""), None));
                rules.push(lines.len());
                lines.push((rule(w), None));
                lines.push((Line::raw(""), None));
                match &page.about {
                    None => lines.push((Line::styled("  ABOUT  loading…", dim), None)),
                    Some((_, Err(e))) => lines.push((Line::styled(format!("  ABOUT  {e}"), dim), None)),
                    Some((_, Ok(a))) => {
                        let open = page.stats.as_ref().and_then(|s| s.as_ref().ok()).map_or((0, 0), |s| (s.prs[0], s.issues[0]));
                        let (more, at) = about_lines(a, open, w, link, now);
                        let base = lines.len();
                        charts.extend(at.into_iter().map(|c| ChartAt { line: base + c.line, ..c }));
                        lines.extend(more);
                    }
                }
                lines.push((Line::raw(""), None));
                match &page.stats {
                    None => lines.push((Line::styled("  INSIGHTS  working them out…", dim), None)),
                    Some(Err(e)) => lines.push((Line::styled(format!("  INSIGHTS  {e}"), dim), None)),
                    Some(Ok(st)) => lines.extend(insights(st, w).into_iter().map(|l| (l, None))),
                }
            }
            if let Some((name, text)) = &d.readme {
                lines.push((Line::raw(""), None));
                lines.push((Line::from(vec![Span::styled(format!("  {} ", icons().outline), dim), Span::styled(name.clone(), Style::new().fg(P.editor_fg).bold())]), None));
                rules.push(lines.len());
                lines.push((rule(w), None));
                let width = area.width.saturating_sub(4);
                if page.readme.as_ref().is_none_or(|(fo, rw, _)| *fo != page.folder || *rw != width) {
                    let src: Vec<String> = text.lines().map(str::to_string).collect();
                    // Its pictures from where they are kept; those not yet: fetched, then
                    // it is drawn again.
                    let (rev, at) = (page.rev(), page.folder.clone());
                    let missing = std::cell::RefCell::new(Vec::new());
                    let find = |src: &str| {
                        let s = pic_source(src, repo, &rev, &at)?;
                        let p = pic_path(&pictures, &s);
                        if p.is_file() {
                            return Some(p);
                        }
                        missing.borrow_mut().push(s);
                        None
                    };
                    let doc = crate::mdpreview::render_doc(&src, width as usize, link, Some((&find, cell)));
                    wanted = missing.into_inner();
                    page.readme = Some((page.folder.clone(), width, doc));
                }
                let doc = &page.readme.as_ref().expect("rendered above").2;
                readme_pics = Some((lines.len(), doc.pics.clone()));
                for l in &doc.lines {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(l.spans.iter().cloned());
                    lines.push((Line::from(spans), None));
                }
            }
        }
    }
    if !wanted.is_empty() {
        want_pics(app, repo, wanted);
    }
    let n = lines.len();
    let h = area.height as usize;
    let grp = &mut app.groups[g];
    let active = grp.active;
    let v = &mut grp.tabs[active].view;
    v.scroll = v.scroll.min(n.saturating_sub(h));
    let scroll = v.scroll;
    for (row, (line, t)) in lines.into_iter().skip(scroll).take(h).enumerate() {
        let r = Rect::new(area.x, area.y + row as u16, area.width, 1);
        if t.is_some() && hover.is_some_and(|p| r.contains(p)) {
            fill(f, r, P.list_hover_bg);
        }
        f.render_widget(line, r);
        if let Some(t) = t {
            app.gh.hits.page.push((r, t));
        }
    }
    // The README's pictures (two columns in, as its lines), in pixels where they can be.
    if let Some((first, pics)) = readme_pics {
        let pics: Vec<_> = pics.into_iter().map(|p| crate::mdpreview::Pic { row: p.row + first, ..p }).collect();
        crate::ui::panels::md_pictures(app, f, &pics, Rect { x: area.x + 2, width: area.width.saturating_sub(2), ..area }, scroll, cell);
    }
    // With Kitty graphics, the traffic bars again in pixels over their cells: the rows
    // of them on screen.
    if let Some(cell) = crate::media::pixel_cell(app).filter(|_| !charts.is_empty() || !rules.is_empty()) {
        // The rules as one crisp pixel line through their row's middle.
        let cols = area.width.saturating_sub(4);
        for (k, &line) in rules.iter().enumerate().filter(|(_, l)| (scroll..scroll + h).contains(*l) && cols > 0) {
            let (w, ch) = (u32::from(cols) * u32::from(cell.0), u32::from(cell.1));
            let crate::theme::Rgba(r, gr, b, a) = crate::ui::pixels::opaque(P.group_border);
            let img = image::RgbaImage::from_fn(w, ch, |_, y| image::Rgba(if y == ch / 2 { [r, gr, b, a] } else { [0; 4] }));
            let at = Rect::new(area.x + 2, area.y + (line - scroll) as u16, cols, 1);
            crate::media::show_pixels(app, f, &format!("rule:{g}:{k}"), at, &img, &|_| P.editor_bg);
        }
        for (k, c) in charts.iter().enumerate() {
            let (top, bottom) = (c.line.max(scroll), (c.line + CHART_ROWS as usize).min(scroll + h));
            let cols = (c.t.days.len() as u16 * DAY_W).min(area.width.saturating_sub(c.x));
            if top >= bottom || cols == 0 {
                continue;
            }
            let img = chart_px(&c.t, cell);
            let (y0, rows) = ((top - c.line) as u32, (bottom - top) as u32);
            let part = image::imageops::crop_imm(&img, 0, y0 * u32::from(cell.1), u32::from(cols) * u32::from(cell.0), rows * u32::from(cell.1)).to_image();
            let r = Rect::new(area.x + c.x, area.y + (top - scroll) as u16, cols, rows as u16);
            crate::media::show_pixels(app, f, &format!("traffic:{g}:{k}"), r, &part, &|_| P.editor_bg);
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_readmes_pictures_come_from_its_repository_or_github_and_no_other_site() {
        let f = |src: &str, folder: &str| pic_source(src, "o/r", "HEAD", folder);
        let file = |repo: &str, rev: &str, path: &str| Some(PicSource::File(repo.into(), rev.into(), path.into()));
        assert_eq!(f("docs/a.png", ""), file("o/r", "HEAD", "docs/a.png"));
        assert_eq!(f("./a.png?raw=1", "sub"), file("o/r", "HEAD", "sub/a.png"));
        assert_eq!(f("../img/a.png", "sub/deep"), file("o/r", "HEAD", "sub/img/a.png"));
        assert_eq!(f("/img/a.png", "sub"), file("o/r", "HEAD", "img/a.png"), "from the root");
        assert_eq!(f("https://raw.githubusercontent.com/x/y/main/a/b.png", ""), file("x/y", "main", "a/b.png"));
        assert_eq!(f("https://github.com/x/y/blob/v2/b.png?raw=true", ""), file("x/y", "v2", "b.png"));
        let url = "https://github.com/user-attachments/assets/1234";
        assert_eq!(f(url, ""), Some(PicSource::Url(url.into())));
        assert!(matches!(f("https://user-images.githubusercontent.com/1/2.png", ""), Some(PicSource::Url(_))));
        for other in ["https://img.shields.io/badge/a-b-c", "http://raw.githubusercontent.com/x/y/m/a.png", "data:image/png;base64,AA", ""] {
            assert_eq!(f(other, ""), None, "{other}");
        }
        // Kept by repository and revision; nothing climbs out of the folder.
        let dir = Path::new("/k");
        assert_eq!(pic_path(dir, &PicSource::File("o/r".into(), "fix/x".into(), "a/../b c.png".into())), Path::new("/k/o_r/fix_x/a/b_c.png"));
        assert_eq!(escaped("a b/ü.png"), "a%20b/%C3%BC.png");
    }

    #[test]
    fn traffic_bars_in_pixels_stand_by_their_counts_their_unique_part_solid() {
        let t = Traffic { all: 12, unique: 3, days: vec![("Nov 10".into(), 4, 1), ("Nov 11".into(), 8, 2), ("Nov 12".into(), 0, 0)] };
        let img = chart_px(&t, (10, 22));
        assert_eq!(img.dimensions(), (3 * u32::from(DAY_W) * 10, u32::from(CHART_ROWS) * 22));
        if let Some(d) = std::env::var_os("TERMCODE_SHOTS") {
            img.save(std::path::Path::new(&d).join("traffic-bars.png")).unwrap();
        }
        let h = img.height();
        let (solid, light) = chart_colors();
        let px = |c: Color| {
            let crate::theme::Rgba(r, g, b, a) = crate::ui::pixels::opaque(c);
            image::Rgba([r, g, b, a])
        };
        // A bar's column: its height, and where solid gives way to light.
        let column = |x: u32| (0..h - 1).rev().take_while(|&y| img.get_pixel(x, y).0[3] > 0 && *img.get_pixel(x, y) != px(P.guide)).count() as u32 + 1;
        let (day0, day1) = (5, u32::from(DAY_W) * 10 + 5);
        assert_eq!(column(day1), h, "the busiest day: to the top");
        assert_eq!(column(day0), h / 2, "half of it: half as tall");
        assert_eq!(*img.get_pixel(day1, h - 2), px(solid), "its unique part at the bottom, solid");
        assert_eq!(*img.get_pixel(day1, 2), px(light), "the rest light");
        assert_eq!(img.get_pixel(2 * u32::from(DAY_W) * 10 + 5, h / 2).0[3], 0, "a day without any: no bar");
    }
}
