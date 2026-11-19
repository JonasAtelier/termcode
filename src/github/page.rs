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
    /// A README rendered for a width: (folder, width, lines).
    readme: Option<(String, u16, Vec<Line<'static>>)>,
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
    let hex = |c: &str| {
        let c = c.trim_start_matches('#');
        let b = |i: usize| u8::from_str_radix(c.get(i..i + 2)?, 16).ok();
        Some(Color::Rgb(b(0)?, b(2)?, b(4)?))
    };
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
    // Lines added and removed a week, as many weeks as fit.
    if st.churn.iter().any(|&(a, d)| a + d > 0) {
        let n = w.saturating_sub(18).min(st.churn.len());
        let weeks = &st.churn[st.churn.len() - n..];
        let max = weeks.iter().map(|&(a, d)| a.max(d)).max().unwrap_or(0).max(1);
        let ticks: Vec<char> = if crate::theme::ascii() { " .:-=+*#".chars().collect() } else { " ▁▂▃▄▅▆▇█".chars().collect() };
        let spark = |x: u64| ticks[((x as f64 / max as f64) * (ticks.len() - 1) as f64).round() as usize];
        let (added, removed): (u64, u64) = weeks.iter().fold((0, 0), |(a, d), w| (a + w.0, d + w.1));
        v.push(Line::raw(""));
        v.push(Line::from(vec![label("Code changes"), Span::styled(format!("+{added} / -{removed} lines, a column a week"), dim)]));
        v.push(Line::from(vec![Span::styled(format!("  {:<14}", "added"), dim), Span::styled(weeks.iter().map(|w| spark(w.0)).collect::<String>(), Style::new().fg(P.gh_green))]));
        v.push(Line::from(vec![Span::styled(format!("  {:<14}", "removed"), dim), Span::styled(weeks.iter().map(|w| spark(w.1)).collect::<String>(), Style::new().fg(P.error_fg))]));
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
        let hot = hover.is_some_and(|p| r.contains(p));
        head.push(Span::raw(" ".repeat(gap)));
        head.push(Span::styled(br, crate::ui::button_style(false, hot)));
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
    if let Some((who, msg, hash, t)) = last.filter(|_| mid.height > 2) {
        let right = if hash.is_empty() { format!("{} ", crate::scm::ago(t, now)) } else { format!("{hash} · {} ", crate::scm::ago(t, now)) };
        let left = if who.is_empty() { "  ".to_string() } else { format!("  {who}  ") };
        let room = (mid.width as usize).saturating_sub(left.width() + right.width() + 1);
        let r = Rect::new(mid.x, mid.y + 1, mid.width, 1);
        fill(f, r, P.widget_bg);
        f.render_widget(
            Line::from(vec![
                Span::styled(left, Style::new().fg(P.widget_fg).bold()),
                Span::styled(format!("{:<room$}", fit(&msg, room)), Style::new().fg(P.widget_fg)),
                Span::raw(" "),
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
        f.render_widget(Span::styled(label, crate::ui::button_style(on_main, hover.is_some_and(|p| br.contains(p)))), br);
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
    want_dir(app, repo, &folder);
    if folder.is_empty() {
        want_stats(app, repo);
    }
    let Some(page) = app.gh.pages.get_mut(repo) else { return 0 };
    let dim = Style::new().fg(P.dim);
    let w = area.width as usize;
    let now = crate::scm::now();
    let mut lines: Vec<(Line<'static>, Option<Target>)> = vec![(Line::raw(""), None)];
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
            // The root's insights, under its files.
            if page.folder.is_empty() {
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
                lines.push((Line::styled(format!("  {}", "─".repeat(w.saturating_sub(4))), Style::new().fg(P.guide)), None));
                let width = area.width.saturating_sub(4);
                if page.readme.as_ref().is_none_or(|(fo, rw, _)| *fo != page.folder || *rw != width) {
                    let src: Vec<String> = text.lines().map(str::to_string).collect();
                    page.readme = Some((page.folder.clone(), width, crate::mdpreview::render(&src, width as usize, link)));
                }
                for l in &page.readme.as_ref().expect("rendered above").2 {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(l.spans.iter().cloned());
                    lines.push((Line::from(spans), None));
                }
            }
        }
    }
    let n = lines.len();
    let h = area.height as usize;
    let grp = &mut app.groups[g];
    let active = grp.active;
    let v = &mut grp.tabs[active].view;
    v.scroll = v.scroll.min(n.saturating_sub(h));
    for (row, (line, t)) in lines.into_iter().skip(v.scroll).take(h).enumerate() {
        let r = Rect::new(area.x, area.y + row as u16, area.width, 1);
        if t.is_some() && hover.is_some_and(|p| r.contains(p)) {
            fill(f, r, P.list_hover_bg);
        }
        f.render_widget(line, r);
        if let Some(t) = t {
            app.gh.hits.page.push((r, t));
        }
    }
    n
}
