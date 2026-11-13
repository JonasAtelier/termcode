//! The GitHub view (the activity bar item above Settings), in two panes. The top one is
//! global: the signed-in account, all its repositories, and its review requests, pull
//! requests, assigned issues and notifications. The bottom one is the repository chosen
//! in it (any, not the folder open here): its description and status, then its Actions
//! runs, pull requests and issues. Everything goes through the GitHub CLI (`gh`), which
//! signs in and keeps the token: termcode never holds one. Signed out, the view only offers
//! to sign in. A change on GitHub (a title, a checkout, a rerun, closing an issue) is asked
//! first.

use crate::panel::{App, Drag, Focus, PAct, Prompt, Rename, Side};
use crate::theme::{icons, P};
use crate::ui::{fit, inset, row};
use image::RgbaImage;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use serde_json::Value;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

// The icon set's `gh` glyphs.
const GITHUB: usize = 0;
const PR: usize = 1;
const PR_DRAFT: usize = 2;
const ISSUE: usize = 3;
const PASS: usize = 4;
const FAIL: usize = 5;
const RUNNING: usize = 6;
const QUEUED: usize = 7;
const SKIPPED: usize = 8;
const EDIT: usize = 9;
const OPEN: usize = 10;
const REFRESH: usize = 11;
const RERUN: usize = 12;
const STOP: usize = 13;
const REPO: usize = 14;
const STAR: usize = 15;
const FORK: usize = 16;

/// How often the panes are fetched again while the view shows (nothing while hidden).
const EVERY: Duration = Duration::from_secs(60);
/// While a sign-in runs in the terminal: how soon to look whether it is done.
const SIGN_IN_EVERY: Duration = Duration::from_secs(3);
/// While GitHub counts a repository's commits (202): how soon, and how often, to ask again.
const STATS_AGAIN: Duration = Duration::from_secs(3);
const STATS_TRIES: u8 = 5;
/// A fetch with no answer by then is given up (its late answer is dropped).
const GIVE_UP: Duration = Duration::from_secs(30);

/// The panes: the account's, and the chosen repository's.
pub const TOP: usize = 0;
pub const BOTTOM: usize = 1;
/// The graphs: the account's contributions, the repository's commits.
const ACCOUNT: usize = 0;
const REPO_GRAPH: usize = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Checks {
    None,
    Pass,
    Fail,
    Pending,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunState {
    Pass,
    Fail,
    Running,
    Queued,
    Skipped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pr {
    /// Its repository, "owner/name".
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub draft: bool,
    /// The branch and checks (a repository's own list has them; a search does not).
    pub branch: String,
    pub checks: Checks,
    pub updated: i64,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub updated: i64,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub id: u64,
    pub title: String,
    pub workflow: String,
    pub branch: String,
    pub state: RunState,
    pub updated: i64,
    pub url: String,
}

/// An unread notification.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub repo: String,
    pub title: String,
    /// "PullRequest", "Issue", "Release", …
    pub kind: String,
    pub updated: i64,
    pub url: String,
}

/// One of the account's repositories, with what the bottom pane says about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoInfo {
    /// "owner/name".
    pub name: String,
    pub description: String,
    pub private: bool,
    pub fork: bool,
    pub archived: bool,
    pub pushed: i64,
    pub stars: u64,
    pub forks: u64,
    pub language: String,
    pub branch: String,
    /// Open issues and pull requests, as GitHub counts them.
    pub open: u64,
    pub url: String,
}

/// A day of the contribution graph.
#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    /// 0 Sunday … 6 Saturday.
    pub weekday: u8,
    pub date: String,
    pub count: u64,
    /// GitHub's level: 0 none, 1-4 its quartiles.
    pub level: u8,
}

/// The account's last year: its contributions by week (Sunday first; the first and last
/// weeks may be short), and who follows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Activity {
    pub total: u64,
    pub weeks: Vec<Vec<Day>>,
    pub followers: u64,
    pub following: u64,
}

/// The account: the top pane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Data {
    pub login: String,
    pub avatar_url: String,
    /// None when GitHub did not give it (the rest still shows).
    pub activity: Option<Activity>,
    /// The year the account was made: the graph goes back that far.
    pub since: i32,
    pub repos: Vec<RepoInfo>,
    pub reviews: Vec<Pr>,
    pub mine: Vec<Pr>,
    pub assigned: Vec<Issue>,
    pub notes: Vec<Note>,
}

/// The chosen repository's lists and numbers: the bottom pane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoData {
    pub prs: Vec<Pr>,
    pub issues: Vec<Issue>,
    pub runs: Vec<Run>,
    /// Its commits a day over the past year; None while GitHub is still counting them.
    pub commits: Option<Activity>,
    /// Its languages by bytes, the most first; its top contributors by commits.
    pub languages: Vec<(String, u64)>,
    pub contributors: Vec<(String, u64)>,
}

/// What the view shows besides data.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// The first fetch is on its way.
    Loading,
    NoGh,
    SignedOut,
    Ready,
    Failed(String),
}

enum Fail {
    NoGh,
    SignedOut,
    Other(String),
}

impl Fail {
    fn text(self) -> String {
        match self {
            Fail::NoGh => "gh is not installed".into(),
            Fail::SignedOut => "not signed in".into(),
            Fail::Other(e) => e,
        }
    }
}

enum Msg {
    Data(u64, Result<Box<Data>, Fail>),
    /// The chosen repository's lists (its name: a later choice drops them).
    Repo(u64, Result<Box<RepoData>, String>),
    Avatar(String, Option<RgbaImage>),
    /// A calendar year's contributions.
    Year(i32, Option<Activity>),
    /// An action finished: what to say.
    Done(Result<String, String>),
}

/// A change on GitHub, run once confirmed.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Title { pr: bool, repo: String, n: u64, title: String },
    Checkout(u64),
    Rerun { repo: String, id: u64, failed: bool },
    Cancel { repo: String, id: u64 },
    Close { repo: String, n: u64 },
}

/// A pull request's or issue's title being edited in its row.
pub struct Edit {
    pub pr: bool,
    pub repo: String,
    pub n: u64,
    pub old: String,
    pub field: Rename,
}

/// The sections: the top pane's, then the bottom pane's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sec {
    Repos,
    Reviews,
    Mine,
    Assigned,
    Notes,
    Runs,
    RepoPrs,
    RepoIssues,
}

const TOP_SECS: [Sec; 5] = [Sec::Repos, Sec::Reviews, Sec::Mine, Sec::Assigned, Sec::Notes];
const BOTTOM_SECS: [Sec; 3] = [Sec::Runs, Sec::RepoPrs, Sec::RepoIssues];

/// A row of a pane's list. A pull request or issue is named by its section's list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Head(Sec),
    Repo(usize),
    Pr(Sec, usize),
    Issue(Sec, usize),
    Run(usize),
    Note(usize),
    Empty(Sec),
}

/// What a row's buttons (and keys) do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Edit,
    Checkout,
    Close,
    Rerun,
    Cancel,
    Open,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Btn {
    SignIn,
    Retry,
    Refresh,
    /// The chosen repository's page.
    OpenRepo,
    /// A row's button: its pane, its row, what it does.
    Act(usize, usize, Key),
    /// The activity header: folds the graph.
    Activity,
    /// The graph by month (true) or by year.
    ActMode(bool),
    /// The repository's stats header: folds them.
    Stats,
    /// An older (-1) or newer (+1) year or month.
    ActStep(i32),
}

#[derive(Default)]
pub struct Hits {
    /// The whole view, each pane's list, each list row (its pane and index), each button,
    /// the line between the panes, and the title field (with how far it scrolled).
    pub area: Rect,
    pub lists: [Rect; 2],
    pub rows: Vec<(Rect, usize, usize)>,
    pub btns: Vec<(Rect, Btn)>,
    pub sash: Rect,
    /// Each pane whole (the top one's top edge: where a drag measures its height from).
    pub panes: [Rect; 2],
    pub field: (Rect, usize),
    /// The contribution graph's blocks (their date and count) and the year view's grid.
    pub act_cells: Vec<(Rect, String, u64, usize)>,
    pub act_graph: [Rect; 2],
}

pub struct GitHub {
    /// The folder's own repository on GitHub, "owner/name": a pull request of it can be
    /// checked out here.
    pub here: Option<String>,
    /// The GitHub CLI to run (tests point it at a fake one).
    pub bin: PathBuf,
    pub state: State,
    pub data: Option<Data>,
    pub loading: bool,
    started: Option<Instant>,
    fetched: Option<Instant>,
    gen: u64,
    /// The repository the bottom pane shows, its lists (or why they did not come).
    pub chosen: Option<String>,
    pub repo: Option<Result<RepoData, String>>,
    pub repo_loading: bool,
    repo_started: Option<Instant>,
    repo_fetched: Option<Instant>,
    repo_gen: u64,
    /// A sign-in is running in the terminal: look again soon.
    pub signing_in: bool,
    /// An action is running (what it is, said in the status bar).
    pub acting: Option<String>,
    pub folded: [bool; 8],
    /// Each pane's selected row and scroll, which pane has the keys, and the top pane's
    /// dragged height.
    pub sel: [usize; 2],
    pub off: [usize; 2],
    /// A key moved the pane's selection: its next frame scrolls it into view (the wheel
    /// scrolls freely meanwhile).
    follow: [bool; 2],
    pub pane: usize,
    pub split: Option<u16>,
    /// The contribution graph shows only its header; so do the repository's stats.
    pub activity_folded: bool,
    pub stats_folded: bool,
    /// Times the repository was fetched again for its commits (GitHub counting them).
    stats_tries: u8,
    /// The graph by month (else by year); the year shown (None: the last 12 months), the
    /// month shown, and how many weeks the year view is scrolled back.
    pub act_month: bool,
    pub act_year: Option<i32>,
    pub act_ym: (i32, u32),
    pub act_scroll: [usize; 2],
    /// Calendar years fetched (a month or a year view), and the one being fetched.
    years: std::collections::HashMap<i32, Activity>,
    year_asked: Option<i32>,
    pub edit: Option<Edit>,
    avatar: Option<(String, RgbaImage)>,
    avatar_asked: Option<String>,
    graphics: Option<crate::settingsgraphics::Graphics>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub hits: Hits,
}

impl GitHub {
    /// `here`: the folder's repository on GitHub; `chosen` and `split` as last saved.
    pub fn new(here: Option<String>, chosen: Option<String>, split: Option<u16>) -> GitHub {
        let (tx, rx) = channel();
        GitHub {
            chosen: chosen.or_else(|| here.clone()),
            here,
            // Tests never reach the real gh (the network, the user's account): theirs is a
            // fake they set.
            bin: PathBuf::from(if cfg!(test) { "/nonexistent/gh" } else { "gh" }),
            state: State::Loading,
            data: None,
            loading: false,
            started: None,
            fetched: None,
            gen: 0,
            repo: None,
            repo_loading: false,
            repo_started: None,
            repo_fetched: None,
            repo_gen: 0,
            signing_in: false,
            acting: None,
            folded: [false; 8],
            sel: [0; 2],
            off: [0; 2],
            follow: [false; 2],
            pane: TOP,
            split,
            activity_folded: false,
            stats_folded: false,
            stats_tries: 0,
            act_month: false,
            act_year: None,
            act_ym: {
                let (y, m, _) = ymd(crate::scm::now());
                (y, m)
            },
            act_scroll: [0; 2],
            years: Default::default(),
            year_asked: None,
            edit: None,
            avatar: None,
            avatar_asked: None,
            graphics: None,
            tx,
            rx,
            hits: Hits::default(),
        }
    }

    /// The account's picture came (or there is none to come).
    #[cfg(test)]
    pub fn avatar_ready(&self) -> bool {
        self.avatar.is_some() || self.avatar_asked.is_none()
    }

    /// Background work is running: the event loop looks often.
    pub fn busy(&self) -> bool {
        self.loading || self.repo_loading || self.acting.is_some() || self.year_asked.is_some() || self.avatar_asked.is_some() && self.avatar.is_none()
    }

    fn lists(&self) -> Option<&RepoData> {
        self.repo.as_ref().and_then(|r| r.as_ref().ok())
    }

    /// The pull requests a section lists.
    pub fn prs(&self, s: Sec) -> &[Pr] {
        match (s, &self.data) {
            (Sec::Reviews, Some(d)) => &d.reviews,
            (Sec::Mine, Some(d)) => &d.mine,
            (Sec::RepoPrs, _) => self.lists().map_or(&[], |r| &r.prs),
            _ => &[],
        }
    }

    pub fn issues(&self, s: Sec) -> &[Issue] {
        match (s, &self.data) {
            (Sec::Assigned, Some(d)) => &d.assigned,
            (Sec::RepoIssues, _) => self.lists().map_or(&[], |r| &r.issues),
            _ => &[],
        }
    }

    pub fn runs(&self) -> &[Run] {
        self.lists().map_or(&[], |r| &r.runs)
    }

    /// The chosen repository's entry in the account's list.
    pub fn chosen_info(&self) -> Option<&RepoInfo> {
        let name = self.chosen.as_deref()?;
        self.data.as_ref()?.repos.iter().find(|r| r.name == name)
    }
}

// ---------------------------------------------------------------- the remote

/// The folder's repository on GitHub, "owner/name": its first remote on github.com (or a
/// host `gh` signed in to, GitHub Enterprise), after git's rewrites. No network.
pub fn detect(root: &Path) -> Option<String> {
    let o = crate::git::cmd(root).args(["remote", "-v"]).stderr(Stdio::null()).output().ok()?;
    let hosts = gh_hosts();
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    // origin first, as gh picks it.
    let mut remotes: Vec<(&str, &str)> = text.lines().filter_map(|l| {
        let mut w = l.split_whitespace();
        Some((w.next()?, w.next()?))
    }).collect();
    remotes.sort_by_key(|(name, _)| *name != "origin");
    remotes.into_iter().map(|(_, url)| url).find_map(|url| {
        let h = host(url)?;
        (h == "github.com" || h == "ssh.github.com" || hosts.contains(&h)).then(|| repo_path(url)).flatten()
    })
}

/// A remote URL's host: `scheme://[user@]host[:port]/path` or scp-like `[user@]host:path`;
/// None for a local path.
pub fn host(url: &str) -> Option<String> {
    let h = match url.split_once("://") {
        Some((_, rest)) => rest.split('/').next()?.rsplit('@').next()?.split(':').next()?,
        None => {
            let (before, _) = url.split_once(':')?;
            if before.contains('/') {
                return None;
            }
            before.rsplit('@').next()?
        }
    };
    (!h.is_empty()).then(|| h.to_ascii_lowercase())
}

/// A remote URL's "owner/name" (its path, without `.git`).
pub fn repo_path(url: &str) -> Option<String> {
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.split_once(':')?.1,
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (path.matches('/').count() == 1).then(|| path.to_string())
}

/// The hosts `gh` is signed in to: hosts.yml's top-level keys.
fn gh_hosts() -> Vec<String> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = var("GH_CONFIG_DIR")
        .or_else(|| var("XDG_CONFIG_HOME").map(|d| d.join("gh")))
        .or_else(|| var("HOME").map(|h| h.join(".config/gh")));
    let text = dir.and_then(|d| std::fs::read_to_string(d.join("hosts.yml")).ok()).unwrap_or_default();
    text.lines()
        .filter(|l| !l.starts_with([' ', '\t', '#']))
        .filter_map(|l| l.strip_suffix(':'))
        .map(|h| h.trim().to_ascii_lowercase())
        .collect()
}

// ---------------------------------------------------------------- talking to gh

fn command(bin: &Path, root: &Path) -> Command {
    let mut c = Command::new(bin);
    c.current_dir(root)
        .stdin(Stdio::null())
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_SPINNER_DISABLED", "1")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0");
    // Its own process group, as git's: Ctrl+C in a terminal never reaches it.
    c.process_group(0);
    c
}

/// `gh ARGS` in `root`: its output. Exit 4 is gh's "not signed in".
fn gh(bin: &Path, root: &Path, args: &[&str]) -> Result<Vec<u8>, Fail> {
    let out = match command(bin, root).args(args).output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Fail::NoGh),
        Err(e) => return Err(Fail::Other(e.to_string())),
    };
    if out.status.code() == Some(4) {
        return Err(Fail::SignedOut);
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("gh failed");
        return Err(Fail::Other(line.to_string()));
    }
    Ok(out.stdout)
}

fn gh_json(bin: &Path, root: &Path, args: &[&str]) -> Result<Value, Fail> {
    serde_json::from_slice(&gh(bin, root, args)?).map_err(|e| Fail::Other(format!("gh {}: {e}", args.first().unwrap_or(&""))))
}

const SEARCH_PR: &str = "number,title,author,repository,updatedAt,url,isDraft";
const SEARCH_ISSUE: &str = "number,title,author,repository,updatedAt,url";
/// Every repository the account owns, works on or sees through an organization, the last
/// pushed first, a page of 100 at a time; at most this many pages.
const REPOS: &str = "user/repos?per_page=100&sort=pushed&affiliation=owner,collaborator,organization_member";
const REPO_PAGES: usize = 5;

/// The account's repositories, page by page until one comes short.
fn all_repos(bin: &Path, root: &Path) -> Result<Value, Fail> {
    let mut all = Vec::new();
    for page in 1..=REPO_PAGES {
        let v = gh_json(bin, root, &["api", &format!("{REPOS}&page={page}")])?;
        let got = v.as_array().map_or(0, Vec::len);
        all.extend(v.as_array().cloned().unwrap_or_default());
        if got < 100 {
            break;
        }
    }
    Ok(Value::Array(all))
}

/// The contribution graph and the follower counts, in one GraphQL query.
const ACTIVITY: &str = "query=query { viewer { followers { totalCount } following { totalCount } \
    contributionsCollection { contributionCalendar { totalContributions \
    weeks { contributionDays { contributionCount contributionLevel date weekday } } } } } }";

/// The account (`gh api user` first: it says whether gh is there and signed in), then side
/// by side its activity, repositories, searches and notifications.
fn load(bin: &Path, root: &Path) -> Result<Data, Fail> {
    let user = gh_json(bin, root, &["api", "user"])?;
    let q = |args: &[&str]| gh_json(bin, root, args);
    std::thread::scope(|s| {
        let activity = s.spawn(|| q(&["api", "graphql", "-f", ACTIVITY]));
        let repos = s.spawn(|| all_repos(bin, root));
        let reviews = s.spawn(|| q(&["search", "prs", "--review-requested", "@me", "--state", "open", "--limit", "30", "--json", SEARCH_PR]));
        let mine = s.spawn(|| q(&["search", "prs", "--author", "@me", "--state", "open", "--limit", "30", "--json", SEARCH_PR]));
        let assigned = s.spawn(|| q(&["search", "issues", "--assignee", "@me", "--state", "open", "--limit", "30", "--json", SEARCH_ISSUE]));
        let unread = s.spawn(|| q(&["api", "notifications"]));
        let join = |h: std::thread::ScopedJoinHandle<'_, Result<Value, Fail>>| h.join().unwrap_or_else(|_| Err(Fail::Other("gh failed".into())));
        Ok(Data {
            login: str_of(&user, "login"),
            avatar_url: str_of(&user, "avatar_url"),
            activity: join(activity).ok().and_then(|v| activity_of(&v)),
            since: ymd(iso_time(&str_of(&user, "created_at"))).0,
            repos: repos_of(&join(repos)?),
            reviews: prs(&join(reviews)?, ""),
            mine: prs(&join(mine)?, ""),
            assigned: issues(&join(assigned)?, ""),
            notes: notes(&join(unread)?),
        })
    })
}

/// The chosen repository's runs, pull requests and issues, side by side.
fn load_repo(bin: &Path, root: &Path, name: &str) -> Result<RepoData, String> {
    let q = |args: &[&str]| gh_json(bin, root, args).map_err(Fail::text);
    std::thread::scope(|s| {
        let p = s.spawn(|| q(&["pr", "list", "--repo", name, "--limit", "30", "--json", "number,title,author,isDraft,headRefName,statusCheckRollup,updatedAt,url"]));
        let i = s.spawn(|| q(&["issue", "list", "--repo", name, "--limit", "30", "--json", "number,title,author,updatedAt,url"]));
        let r = s.spawn(|| q(&["run", "list", "--repo", name, "--limit", "20", "--json", "databaseId,displayTitle,workflowName,headBranch,status,conclusion,updatedAt,url"]));
        let join = |h: std::thread::ScopedJoinHandle<'_, Result<Value, String>>| h.join().unwrap_or_else(|_| Err("gh failed".into()));
        // Its numbers: missing ones only leave their row out.
        let commits = s.spawn(|| q(&["api", &format!("repos/{name}/stats/commit_activity")]));
        let langs = s.spawn(|| q(&["api", &format!("repos/{name}/languages")]));
        let people = s.spawn(|| q(&["api", &format!("repos/{name}/contributors?per_page=5")]));
        let (p, i, r) = (join(p), join(i), join(r));
        let (commits, langs, people) = (join(commits), join(langs), join(people));
        Ok(RepoData {
            prs: prs(&p?, name),
            issues: issues(&i?, name),
            runs: runs(&r?),
            commits: commits.ok().and_then(|v| commits_of(&v)),
            languages: langs.ok().map(|v| languages_of(&v)).unwrap_or_default(),
            contributors: people.ok().map(|v| contributors_of(&v)).unwrap_or_default(),
        })
    })
}

fn str_of(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

/// The activity from the GraphQL answer.
pub fn activity_of(v: &Value) -> Option<Activity> {
    let viewer = &v["data"]["viewer"];
    let cal = &viewer["contributionsCollection"]["contributionCalendar"];
    let level = |l: &str| match l {
        "FIRST_QUARTILE" => 1,
        "SECOND_QUARTILE" => 2,
        "THIRD_QUARTILE" => 3,
        "FOURTH_QUARTILE" => 4,
        _ => 0,
    };
    let weeks = cal["weeks"].as_array()?;
    Some(Activity {
        total: cal["totalContributions"].as_u64().unwrap_or(0),
        weeks: weeks
            .iter()
            .map(|w| {
                let days = w["contributionDays"].as_array().cloned().unwrap_or_default();
                days.iter()
                    .map(|d| Day {
                        weekday: d["weekday"].as_u64().unwrap_or(0).min(6) as u8,
                        date: str_of(d, "date"),
                        count: d["contributionCount"].as_u64().unwrap_or(0),
                        level: level(d["contributionLevel"].as_str().unwrap_or("")),
                    })
                    .collect()
            })
            .collect(),
        followers: viewer["followers"]["totalCount"].as_u64().unwrap_or(0),
        following: viewer["following"]["totalCount"].as_u64().unwrap_or(0),
    })
}

/// A repository's commits a day from its weekly commit activity (52 weeks of 7 days from
/// Sunday); None while GitHub is still counting (it answers `{}` with 202 meanwhile).
pub fn commits_of(v: &Value) -> Option<Activity> {
    let weeks = v.as_array().filter(|a| !a.is_empty())?;
    let counts = |w: &Value| w["days"].as_array().cloned().unwrap_or_default().iter().map(|d| d.as_u64().unwrap_or(0)).collect::<Vec<u64>>();
    let max = weeks.iter().flat_map(counts).max().unwrap_or(0).max(1);
    // Levels as GitHub's graph has them: four steps up to the busiest day.
    let level = |c: u64| if c == 0 { 0 } else { (4 * c).div_ceil(max).clamp(1, 4) as u8 };
    let weeks: Vec<Vec<Day>> = weeks
        .iter()
        .map(|w| {
            let start = w["week"].as_i64().unwrap_or(0);
            counts(w)
                .into_iter()
                .enumerate()
                .map(|(i, count)| {
                    let (y, m, d) = ymd(start + i as i64 * 86_400);
                    Day { weekday: i as u8, date: format!("{y}-{m:02}-{d:02}"), count, level: level(count) }
                })
                .collect()
        })
        .collect();
    let total = weeks.iter().flatten().map(|d| d.count).sum();
    Some(Activity { total, weeks, ..Activity::default() })
}

/// A repository's languages by bytes, the most first.
pub fn languages_of(v: &Value) -> Vec<(String, u64)> {
    let mut l: Vec<(String, u64)> = v.as_object().into_iter().flatten().map(|(k, n)| (k.clone(), n.as_u64().unwrap_or(0))).collect();
    l.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    l
}

pub fn contributors_of(v: &Value) -> Vec<(String, u64)> {
    v.as_array().into_iter().flatten().map(|c| (str_of(c, "login"), c["contributions"].as_u64().unwrap_or(0))).collect()
}

/// A language's colour, as GitHub (linguist) draws it; others a colour of their name.
fn lang_color(lang: &str) -> Color {
    let hex = match lang {
        "Rust" => 0xdea584,
        "Shell" => 0x89e051,
        "Python" => 0x3572a5,
        "C" => 0x555555,
        "C++" => 0xf34b7d,
        "C#" => 0x178600,
        "Go" => 0x00add8,
        "JavaScript" => 0xf1e05a,
        "TypeScript" => 0x3178c6,
        "HTML" => 0xe34c26,
        "CSS" => 0x563d7c,
        "Java" => 0xb07219,
        "Kotlin" => 0xa97bff,
        "Ruby" => 0x701516,
        "Swift" => 0xf05138,
        "Lua" => 0x000080,
        "Makefile" => 0x427819,
        "CMake" => 0xda3434,
        "Dockerfile" => 0x384d54,
        "Assembly" => 0x6e4c13,
        "Perl" => 0x0298c3,
        "Nix" => 0x7e7eff,
        "Vim Script" => 0x199f4b,
        "Jupyter Notebook" => 0xda5b0b,
        _ => {
            const OTHERS: [u32; 6] = [0x4f9ddb, 0xc586c0, 0xd7ba7d, 0x4ec9b0, 0xce9178, 0x9cdcfe];
            OTHERS[lang.bytes().map(usize::from).sum::<usize>() % OTHERS.len()]
        }
    };
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The account's repositories from the REST API's JSON.
pub fn repos_of(v: &Value) -> Vec<RepoInfo> {
    let list = v.as_array().cloned().unwrap_or_default();
    list.iter()
        .map(|r| RepoInfo {
            name: str_of(r, "full_name"),
            description: str_of(r, "description"),
            private: r["private"].as_bool().unwrap_or(false),
            fork: r["fork"].as_bool().unwrap_or(false),
            archived: r["archived"].as_bool().unwrap_or(false),
            pushed: iso_time(&str_of(r, "pushed_at")),
            stars: r["stargazers_count"].as_u64().unwrap_or(0),
            forks: r["forks_count"].as_u64().unwrap_or(0),
            language: str_of(r, "language"),
            branch: str_of(r, "default_branch"),
            open: r["open_issues_count"].as_u64().unwrap_or(0),
            url: str_of(r, "html_url"),
        })
        .collect()
}

/// Pull requests from gh's JSON: a search's carry their repository, a repository's own
/// list (`repo`) its branch and checks.
pub fn prs(v: &Value, repo: &str) -> Vec<Pr> {
    let list = v.as_array().cloned().unwrap_or_default();
    list.iter()
        .map(|p| Pr {
            repo: if repo.is_empty() { str_of(&p["repository"], "nameWithOwner") } else { repo.to_string() },
            number: p["number"].as_u64().unwrap_or(0),
            title: str_of(p, "title"),
            author: str_of(&p["author"], "login"),
            draft: p["isDraft"].as_bool().unwrap_or(false),
            branch: str_of(p, "headRefName"),
            checks: rollup(&p["statusCheckRollup"]),
            updated: iso_time(&str_of(p, "updatedAt")),
            url: str_of(p, "url"),
        })
        .collect()
}

pub fn issues(v: &Value, repo: &str) -> Vec<Issue> {
    let list = v.as_array().cloned().unwrap_or_default();
    list.iter()
        .map(|i| Issue {
            repo: if repo.is_empty() { str_of(&i["repository"], "nameWithOwner") } else { repo.to_string() },
            number: i["number"].as_u64().unwrap_or(0),
            title: str_of(i, "title"),
            author: str_of(&i["author"], "login"),
            updated: iso_time(&str_of(i, "updatedAt")),
            url: str_of(i, "url"),
        })
        .collect()
}

pub fn runs(v: &Value) -> Vec<Run> {
    let list = v.as_array().cloned().unwrap_or_default();
    list.iter()
        .map(|r| Run {
            id: r["databaseId"].as_u64().unwrap_or(0),
            title: str_of(r, "displayTitle"),
            workflow: str_of(r, "workflowName"),
            branch: str_of(r, "headBranch"),
            state: run_state(&str_of(r, "status"), &str_of(r, "conclusion")),
            updated: iso_time(&str_of(r, "updatedAt")),
            url: str_of(r, "url"),
        })
        .collect()
}

/// Unread notifications, each with a link to its page (from the API's).
pub fn notes(v: &Value) -> Vec<Note> {
    let list = v.as_array().cloned().unwrap_or_default();
    list.iter()
        .map(|n| {
            let repo = &n["repository"];
            Note {
                repo: str_of(repo, "full_name"),
                title: str_of(&n["subject"], "title"),
                kind: str_of(&n["subject"], "type"),
                updated: iso_time(&str_of(n, "updated_at")),
                url: page_url(&str_of(&n["subject"], "url")).unwrap_or_else(|| str_of(repo, "html_url")),
            }
        })
        .collect()
}

/// The web page of an API URL: `https://api.github.com/repos/o/r/pulls/12` ->
/// `https://github.com/o/r/pull/12` (GitHub Enterprise: `HOST/api/v3/repos/…`).
pub fn page_url(api: &str) -> Option<String> {
    let (host, rest) = if let Some(rest) = api.strip_prefix("https://api.github.com/repos/") {
        ("https://github.com".to_string(), rest)
    } else {
        let (base, rest) = api.split_once("/api/v3/repos/")?;
        (base.to_string(), rest)
    };
    Some(format!("{host}/{}", rest.replacen("/pulls/", "/pull/", 1).replacen("/commits/", "/commit/", 1)))
}

/// A pull request's checks as one: any failed, else any still running, else passed.
fn rollup(v: &Value) -> Checks {
    let Some(checks) = v.as_array().filter(|a| !a.is_empty()) else { return Checks::None };
    let word = |c: &Value, k: &str| c[k].as_str().unwrap_or("").to_ascii_uppercase();
    let failed = |c: &Value| {
        matches!(word(c, "conclusion").as_str(), "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE")
            || matches!(word(c, "state").as_str(), "FAILURE" | "ERROR")
    };
    let pending = |c: &Value| {
        let status = word(c, "status");
        (!status.is_empty() && status != "COMPLETED") || matches!(word(c, "state").as_str(), "PENDING" | "EXPECTED")
    };
    if checks.iter().any(failed) {
        Checks::Fail
    } else if checks.iter().any(pending) {
        Checks::Pending
    } else {
        Checks::Pass
    }
}

fn run_state(status: &str, conclusion: &str) -> RunState {
    match (status, conclusion) {
        ("completed", "success") => RunState::Pass,
        ("completed", "failure" | "timed_out" | "startup_failure" | "action_required") => RunState::Fail,
        ("completed", _) => RunState::Skipped,
        ("in_progress", _) => RunState::Running,
        _ => RunState::Queued,
    }
}

/// Unix seconds for an ISO 8601 UTC time ("2026-11-13T03:04:05Z"); 0 when it is not one.
pub fn iso_time(s: &str) -> i64 {
    let num = |a: usize, b: usize| s.get(a..b).and_then(|t| t.parse::<i64>().ok());
    let (Some(y), Some(m), Some(d), Some(hh), Some(mm), Some(ss)) = (num(0, 4), num(5, 7), num(8, 10), num(11, 13), num(14, 16), num(17, 19)) else { return 0 };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss
}

/// Where gh runs: the repository, else the folder (its lists name their repository).
fn cwd(app: &App) -> PathBuf {
    app.git_root.clone().unwrap_or_else(|| app.root.clone())
}

/// Fetch the account again (what shows stays meanwhile), and the chosen repository.
pub fn fetch(app: &mut App) {
    let root = cwd(app);
    let gh = &mut app.gh;
    gh.gen += 1;
    gh.loading = true;
    gh.started = Some(Instant::now());
    let (gen, bin, tx) = (gh.gen, gh.bin.clone(), gh.tx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(Msg::Data(gen, load(&bin, &root).map(Box::new)));
    });
    fetch_repo(app);
}

/// Fetch the chosen repository's lists.
pub fn fetch_repo(app: &mut App) {
    let root = cwd(app);
    let gh = &mut app.gh;
    let Some(name) = gh.chosen.clone() else { return };
    gh.repo_gen += 1;
    gh.repo_loading = true;
    gh.repo_started = Some(Instant::now());
    let (gen, bin, tx) = (gh.repo_gen, gh.bin.clone(), gh.tx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(Msg::Repo(gen, load_repo(&bin, &root, &name).map(Box::new)));
    });
}

/// The bottom pane shows `name` (remembered in the layout).
pub fn choose(app: &mut App, name: &str) {
    if app.gh.chosen.as_deref() == Some(name) {
        return;
    }
    let gh = &mut app.gh;
    (gh.chosen, gh.repo, gh.sel[BOTTOM], gh.off[BOTTOM]) = (Some(name.to_string()), None, 0, 0);
    (gh.stats_tries, gh.act_scroll[REPO_GRAPH]) = (0, 0);
    gh.edit = None;
    fetch_repo(app);
    app.save_layout();
}

/// Fold finished work in and start what is due; true when the view changes. Nothing is
/// fetched while the view is hidden.
pub fn tick(app: &mut App) -> bool {
    let mut changed = false;
    while let Ok(m) = app.gh.rx.try_recv() {
        changed = true;
        match m {
            Msg::Data(gen, res) if gen == app.gh.gen => {
                let gh = &mut app.gh;
                (gh.loading, gh.fetched) = (false, Some(Instant::now()));
                match res {
                    Ok(d) => {
                        if gh.signing_in {
                            app.msg = format!("signed in to GitHub as {}", d.login);
                        }
                        let gh = &mut app.gh;
                        gh.signing_in = false;
                        gh.state = State::Ready;
                        gh.data = Some(*d);
                    }
                    Err(Fail::NoGh) => gh.state = State::NoGh,
                    Err(Fail::SignedOut) => gh.state = State::SignedOut,
                    Err(Fail::Other(e)) => gh.state = State::Failed(e),
                }
                want_avatar(app);
            }
            Msg::Repo(gen, res) if gen == app.gh.repo_gen => {
                let gh = &mut app.gh;
                (gh.repo_loading, gh.repo_fetched) = (false, Some(Instant::now()));
                gh.repo = Some(res.map(|r| *r));
            }
            Msg::Data(..) | Msg::Repo(..) => {}
            Msg::Avatar(login, img) => {
                if let Some(img) = img {
                    app.gh.avatar = Some((login, img));
                }
            }
            Msg::Year(y, a) => {
                let gh = &mut app.gh;
                gh.year_asked = None;
                // A year that did not come shows as empty (a click on ‹ › asks again).
                gh.years.insert(y, a.unwrap_or_default());
            }
            Msg::Done(res) => {
                app.gh.acting = None;
                app.msg = res.unwrap_or_else(|e| format!("GitHub: {e}"));
                fetch(app);
                // A checkout moves the branch: the editor and Source Control follow.
                app.refresh();
            }
        }
    }
    let shown = app.side == Side::GitHub && app.side_visible;
    let gh = &mut app.gh;
    for (loading, started, fetched, data) in [(&mut gh.loading, &gh.started, &mut gh.fetched, gh.data.is_some()), (&mut gh.repo_loading, &gh.repo_started, &mut gh.repo_fetched, true)] {
        if *loading && started.is_some_and(|t| t.elapsed() >= GIVE_UP) {
            (*loading, *fetched) = (false, Some(Instant::now()));
            changed = true;
            if !data {
                gh.state = State::Failed("GitHub did not answer".into());
            }
        }
    }
    let every = if gh.signing_in {
        Some(SIGN_IN_EVERY)
    } else {
        shown.then_some(EVERY)
    };
    if let Some(every) = every {
        if !gh.loading && gh.fetched.is_none_or(|t| t.elapsed() >= every) {
            fetch(app);
            changed = true;
        } else if gh.state == State::Ready && !gh.repo_loading && gh.chosen.is_some() && gh.repo_fetched.is_none_or(|t| t.elapsed() >= every) {
            fetch_repo(app);
            changed = true;
        } else if !gh.repo_loading && gh.stats_tries < STATS_TRIES && gh.lists().is_some_and(|r| r.commits.is_none()) && gh.repo_fetched.is_some_and(|t| t.elapsed() >= STATS_AGAIN) {
            // GitHub was counting the commits: ask again shortly.
            gh.stats_tries += 1;
            fetch_repo(app);
            changed = true;
        }
    }
    changed
}

/// The signed-in account's picture, once per account (gh fetches it, as it does the rest).
fn want_avatar(app: &mut App) {
    let Some(d) = app.gh.data.as_ref() else { return };
    if d.avatar_url.is_empty() || app.gh.avatar_asked.as_deref() == Some(d.login.as_str()) {
        return;
    }
    let login = d.login.clone();
    let url = format!("{}{}s=64", d.avatar_url, if d.avatar_url.contains('?') { '&' } else { '?' });
    app.gh.avatar_asked = Some(login.clone());
    let (bin, tx, root) = (app.gh.bin.clone(), app.gh.tx.clone(), cwd(app));
    std::thread::spawn(move || {
        let img = gh(&bin, &root, &["api", &url]).ok().and_then(|b| image::load_from_memory(&b).ok()).map(|i| i.to_rgba8());
        let _ = tx.send(Msg::Avatar(login, img));
    });
}

// ---------------------------------------------------------------- actions

/// Sign in: `gh auth login --web` in the terminal (it shows a code to enter in the
/// browser); the view looks every few seconds whether it is done.
pub fn sign_in(app: &mut App) {
    if app.term.is_none() || !app.term_open {
        app.toggle_terminal();
    }
    let cmd = format!("{} auth login --web\r", app.gh.bin.display());
    if let Some(t) = app.term.as_mut() {
        t.send(cmd.as_bytes());
        app.focus = Focus::Term;
        app.gh.signing_in = true;
        app.msg = "signing in to GitHub in the terminal…".into();
    }
}

/// Open `url` in the browser; with none (over SSH), the link goes to the clipboard.
fn open_url(app: &mut App, url: &str) {
    let opened = Command::new("xdg-open").arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    match opened {
        Ok(c) if !cfg!(test) => {
            crate::nav::reap(c, None);
            app.msg = format!("opened {url}");
        }
        res => {
            if let Ok(c) = res {
                crate::nav::reap(c, None);
            }
            app.msg = format!("no browser here: copied the link via {}", crate::panel::clipboard_set(url));
            app.clip = url.to_string();
        }
    }
}

/// What `row`'s buttons offer, left to right. A pull request checks out only into the
/// folder's own repository.
pub fn keys_of(gh: &GitHub, row: Row) -> Vec<Key> {
    match row {
        Row::Repo(_) => vec![Key::Open],
        Row::Pr(s, i) => match gh.prs(s).get(i) {
            Some(p) if s == Sec::RepoPrs && gh.here.as_deref() == Some(p.repo.as_str()) => vec![Key::Edit, Key::Checkout, Key::Open],
            Some(_) => vec![Key::Edit, Key::Open],
            None => Vec::new(),
        },
        Row::Issue(..) => vec![Key::Edit, Key::Close, Key::Open],
        Row::Run(i) => match gh.runs().get(i).map(|r| r.state) {
            Some(RunState::Running | RunState::Queued) => vec![Key::Cancel, Key::Open],
            Some(_) => vec![Key::Rerun, Key::Open],
            None => Vec::new(),
        },
        Row::Note(_) => vec![Key::Open],
        _ => Vec::new(),
    }
}

/// Asks before a change on GitHub: `what` (and `why`), its button, and the change.
fn ask(app: &mut App, what: String, why: String, button: &str, act: Act) {
    app.prompt = Some(Prompt { text: vec![what, why], buttons: vec![(button.into(), PAct::GitHub(act)), ("Cancel".into(), PAct::Cancel)], sel: 0 });
}

/// A row's button or key: open it, edit its title, or ask before changing it.
fn act(app: &mut App, row: Row, key: Key) {
    if !keys_of(&app.gh, row).contains(&key) {
        return;
    }
    let gh = &app.gh;
    let chosen = gh.chosen.clone().unwrap_or_default();
    match (row, key) {
        (Row::Repo(i), Key::Open) => {
            let url = gh.data.as_ref().map(|d| d.repos[i].url.clone()).unwrap_or_default();
            open_url(app, &url);
        }
        (Row::Pr(s, i), Key::Open) => {
            let url = gh.prs(s)[i].url.clone();
            open_url(app, &url);
        }
        (Row::Issue(s, i), Key::Open) => {
            let url = gh.issues(s)[i].url.clone();
            open_url(app, &url);
        }
        (Row::Run(i), Key::Open) => {
            let url = gh.runs()[i].url.clone();
            open_url(app, &url);
        }
        (Row::Note(i), Key::Open) => {
            let url = gh.data.as_ref().map(|d| d.notes[i].url.clone()).unwrap_or_default();
            open_url(app, &url);
        }
        (Row::Pr(s, i), Key::Edit) => {
            let p = gh.prs(s)[i].clone();
            start_edit(app, true, &p.repo, p.number, &p.title);
        }
        (Row::Issue(s, i), Key::Edit) => {
            let is = gh.issues(s)[i].clone();
            start_edit(app, false, &is.repo, is.number, &is.title);
        }
        (Row::Pr(s, i), Key::Checkout) => {
            let p = gh.prs(s)[i].clone();
            let why = format!("Your working tree switches to its branch, {}.", p.branch);
            ask(app, format!("Check out pull request #{}?", p.number), why, "Check Out", Act::Checkout(p.number));
        }
        (Row::Issue(s, i), Key::Close) => {
            let is = gh.issues(s)[i].clone();
            ask(app, format!("Close issue #{} in {}?", is.number, is.repo), format!("\"{}\"", is.title), "Close Issue", Act::Close { repo: is.repo, n: is.number });
        }
        (Row::Run(i), Key::Rerun) => {
            let r = gh.runs()[i].clone();
            let failed = r.state == RunState::Fail;
            let why = if failed { "Only its failed jobs run again.".to_string() } else { "All its jobs run again.".to_string() };
            ask(app, format!("Re-run {} \"{}\"?", r.workflow, r.title), why, "Re-run", Act::Rerun { repo: chosen, id: r.id, failed });
        }
        (Row::Run(i), Key::Cancel) => {
            let r = gh.runs()[i].clone();
            ask(app, format!("Cancel {} \"{}\"?", r.workflow, r.title), "Its jobs stop where they are.".into(), "Cancel Run", Act::Cancel { repo: chosen, id: r.id });
        }
        _ => {}
    }
}

fn start_edit(app: &mut App, pr: bool, repo: &str, n: u64, title: &str) {
    let field = Rename { path: String::new(), text: title.to_string(), cur: title.len(), anchor: Some(0) };
    app.gh.edit = Some(Edit { pr, repo: repo.to_string(), n, old: title.to_string(), field });
    app.focus = Focus::Side;
}

/// Enter in the title field: asks before changing it (an unchanged or empty one ends it).
fn finish_edit(app: &mut App) {
    let Some(e) = app.gh.edit.as_ref() else { return };
    let title = e.field.text.trim().to_string();
    if title.is_empty() || title == e.old {
        app.gh.edit = None;
        return;
    }
    let what = if e.pr { "pull request" } else { "issue" };
    let why = format!("\"{}\" becomes \"{title}\".", e.old);
    let act = Act::Title { pr: e.pr, repo: e.repo.clone(), n: e.n, title };
    ask(app, format!("Change the title of {what} #{} in {}?", e.n, e.repo), why, "Change Title", act);
}

/// A confirmed change: run it, say so, then fetch again. Each names its repository, so it
/// works from any folder (a checkout goes into the one open here).
pub fn run(app: &mut App, act: Act) {
    let root = cwd(app);
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<String>>();
    let (args, doing, done): (Vec<String>, String, String) = match &act {
        Act::Title { pr, repo, n, title } => {
            let kind = if *pr { "pr" } else { "issue" };
            (s(&[kind, "edit", &n.to_string(), "--repo", repo, "--title", title]), format!("changing the title of #{n}…"), format!("#{n} is now \"{title}\""))
        }
        Act::Checkout(n) => (s(&["pr", "checkout", &n.to_string()]), format!("checking out #{n}…"), format!("checked out #{n}")),
        Act::Rerun { repo, id, failed } => {
            let mut a = s(&["run", "rerun", &id.to_string(), "--repo", repo]);
            if *failed {
                a.push("--failed".into());
            }
            (a, "starting the run again…".into(), "the run started again".into())
        }
        Act::Cancel { repo, id } => (s(&["run", "cancel", &id.to_string(), "--repo", repo]), "cancelling the run…".into(), "the run is cancelled".into()),
        Act::Close { repo, n } => (s(&["issue", "close", &n.to_string(), "--repo", repo]), format!("closing #{n}…"), format!("closed #{n}")),
    };
    if matches!(act, Act::Title { .. }) {
        app.gh.edit = None;
    }
    app.msg = doing.clone();
    app.gh.acting = Some(doing);
    let (bin, tx) = (app.gh.bin.clone(), app.gh.tx.clone());
    std::thread::spawn(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let res = gh(&bin, &root, &args).map(|_| done).map_err(Fail::text);
        let _ = tx.send(Msg::Done(res));
    });
}

// ---------------------------------------------------------------- the lists

/// A pane's list: each section's header, then its rows (none while folded, a note when
/// empty). The bottom pane's needs a chosen repository whose lists came.
pub fn rows(gh: &GitHub, pane: usize) -> Vec<Row> {
    let Some(d) = &gh.data else { return Vec::new() };
    if pane == BOTTOM && gh.lists().is_none() {
        return Vec::new();
    }
    let mut v = Vec::new();
    for s in if pane == TOP { &TOP_SECS[..] } else { &BOTTOM_SECS[..] } {
        let s = *s;
        v.push(Row::Head(s));
        if gh.folded[s as usize] {
            continue;
        }
        let before = v.len();
        match s {
            Sec::Repos => v.extend((0..d.repos.len()).map(Row::Repo)),
            Sec::Reviews | Sec::Mine | Sec::RepoPrs => v.extend((0..gh.prs(s).len()).map(|i| Row::Pr(s, i))),
            Sec::Assigned | Sec::RepoIssues => v.extend((0..gh.issues(s).len()).map(|i| Row::Issue(s, i))),
            Sec::Notes => v.extend((0..d.notes.len()).map(Row::Note)),
            Sec::Runs => v.extend((0..gh.runs().len()).map(Row::Run)),
        }
        if v.len() == before {
            v.push(Row::Empty(s));
        }
    }
    v
}

/// Keys while the view has them; false for keys it leaves to the rest of termcode. Tab
/// moves between the panes.
pub fn key(app: &mut App, k: KeyEvent) -> bool {
    if app.side != Side::GitHub {
        return false;
    }
    if app.gh.edit.is_some() {
        match k.code {
            KeyCode::Enter => finish_edit(app),
            KeyCode::Esc => app.gh.edit = None,
            _ => {
                if let Some(e) = app.gh.edit.as_mut() {
                    e.field.key(k);
                }
            }
        }
        return true;
    }
    let ctrl = k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match app.gh.state {
        State::SignedOut if k.code == KeyCode::Enter => {
            sign_in(app);
            return true;
        }
        State::NoGh | State::Failed(_) if k.code == KeyCode::Enter => {
            fetch(app);
            return true;
        }
        State::Ready => {}
        _ => return false,
    }
    match k.code {
        KeyCode::F(5) => {
            fetch(app);
            return true;
        }
        KeyCode::Tab | KeyCode::BackTab if !ctrl => {
            app.gh.pane = 1 - app.gh.pane;
            return true;
        }
        _ => {}
    }
    let pane = app.gh.pane;
    app.gh.follow[pane] = true;
    let list = rows(&app.gh, pane);
    if list.is_empty() || ctrl {
        return k.code != KeyCode::Esc && !ctrl;
    }
    let n = list.len();
    let at = app.gh.sel[pane].min(n - 1);
    let page = app.gh.hits.lists[pane].height.max(1) as usize;
    let row = list[at];
    let go = |gh: &mut GitHub, i: usize| gh.sel[pane] = i.min(n - 1);
    match k.code {
        KeyCode::Up => go(&mut app.gh, at.saturating_sub(1)),
        KeyCode::Down => go(&mut app.gh, at + 1),
        KeyCode::PageUp => go(&mut app.gh, at.saturating_sub(page)),
        KeyCode::PageDown => go(&mut app.gh, at + page),
        KeyCode::Home => go(&mut app.gh, 0),
        KeyCode::End => go(&mut app.gh, n - 1),
        KeyCode::Left => match row {
            Row::Head(s) if !app.gh.folded[s as usize] => app.gh.folded[s as usize] = true,
            _ => {
                if let Some(h) = (0..at).rev().find(|&i| matches!(list[i], Row::Head(_))) {
                    go(&mut app.gh, h);
                }
            }
        },
        KeyCode::Right => match row {
            Row::Head(s) if app.gh.folded[s as usize] => app.gh.folded[s as usize] = false,
            Row::Head(_) => go(&mut app.gh, at + 1),
            _ => {}
        },
        KeyCode::Enter | KeyCode::Char(' ') => enter(app, row),
        KeyCode::Char('e') | KeyCode::F(2) => act(app, row, Key::Edit),
        KeyCode::Char('c') => act(app, row, Key::Checkout),
        KeyCode::Char('x') => {
            let key = if matches!(row, Row::Run(_)) { Key::Cancel } else { Key::Close };
            act(app, row, key);
        }
        KeyCode::Char('r') => act(app, row, Key::Rerun),
        KeyCode::Char('o') => act(app, row, Key::Open),
        KeyCode::Esc => app.focus = Focus::Editor,
        _ => return false,
    }
    true
}

/// Enter (or a double-click): a header folds, a repository becomes the chosen one, any
/// other row opens in the browser.
fn enter(app: &mut App, row: Row) {
    match row {
        Row::Head(s) => app.gh.folded[s as usize] = !app.gh.folded[s as usize],
        Row::Empty(_) => {}
        Row::Repo(i) => {
            if let Some(name) = app.gh.data.as_ref().map(|d| d.repos[i].name.clone()) {
                choose(app, &name);
            }
        }
        r => act(app, r, Key::Open),
    }
}

/// A paste while a title is edited goes into it.
pub fn paste(app: &mut App, text: &str) -> bool {
    match app.gh.edit.as_mut().filter(|_| app.side == Side::GitHub) {
        Some(e) => {
            e.field.paste(text);
            true
        }
        None => false,
    }
}

/// Mouse down: true when the view took it. A click selects a row (a repository: chooses
/// it), a header folds, a double-click opens; the line between the panes drags.
pub fn click(app: &mut App, p: Position, double: bool) -> bool {
    if app.side != Side::GitHub || !app.side_visible || !app.gh.hits.area.contains(p) {
        return false;
    }
    app.focus = Focus::Side;
    if app.gh.hits.field.0.contains(p) {
        return true;
    }
    if app.gh.hits.sash.contains(p) {
        app.drag = Drag::GhSash;
        return true;
    }
    // A day of the year graph: its month.
    if let Some(date) = app.gh.hits.act_cells.iter().find(|(c, .., w)| *w == ACCOUNT && c.contains(p)).map(|(_, d, ..)| d.clone()).filter(|_| !app.gh.act_month) {
        let (y, m) = (date.get(..4).and_then(|y| y.parse().ok()), date.get(5..7).and_then(|m| m.parse().ok()));
        if let (Some(y), Some(m)) = (y, m) {
            (app.gh.act_month, app.gh.act_ym) = (true, (y, m));
        }
        return true;
    }
    if let Some(&(_, b)) = app.gh.hits.btns.iter().find(|(r, _)| r.contains(p)) {
        match b {
            Btn::SignIn => sign_in(app),
            Btn::Retry | Btn::Refresh => fetch(app),
            Btn::Activity => app.gh.activity_folded = !app.gh.activity_folded,
            Btn::Stats => app.gh.stats_folded = !app.gh.stats_folded,
            Btn::ActMode(month) => (app.gh.act_month, app.gh.act_scroll[ACCOUNT]) = (month, 0),
            Btn::ActStep(d) => act_step(app, d),
            Btn::OpenRepo => {
                if let Some(url) = app.gh.chosen_info().map(|r| r.url.clone()) {
                    open_url(app, &url);
                }
            }
            Btn::Act(pane, i, key) => {
                (app.gh.pane, app.gh.sel[pane]) = (pane, i);
                if let Some(&row) = rows(&app.gh, pane).get(i) {
                    act(app, row, key);
                }
            }
        }
        return true;
    }
    if let Some(&(_, pane, i)) = app.gh.hits.rows.iter().find(|(r, ..)| r.contains(p)) {
        (app.gh.pane, app.gh.sel[pane]) = (pane, i);
        match rows(&app.gh, pane).get(i).copied() {
            Some(r @ (Row::Head(_) | Row::Repo(_))) => enter(app, r),
            Some(r) if double => enter(app, r),
            _ => {}
        }
    } else if app.gh.hits.lists[BOTTOM].contains(p) {
        app.gh.pane = BOTTOM;
    }
    true
}

/// The line between the panes, dragged to `y`: the top pane ends there. Measured from the
/// pane's own top edge, which stays put (its list moves as the graph above it comes and
/// goes: measured from there, each frame would undo the last).
pub fn drag_to(app: &mut App, y: u16) {
    let top = app.gh.hits.panes[TOP].y;
    app.gh.split = Some(y.saturating_sub(top).max(1));
}

pub fn wheel(app: &mut App, p: Position, d: isize) -> bool {
    if app.side != Side::GitHub {
        return false;
    }
    // Over the year graph: back (up) and forth in weeks.
    if let Some(w) = (0..2).find(|&w| app.gh.hits.act_graph[w].contains(p)) {
        app.gh.act_scroll[w] = app.gh.act_scroll[w].saturating_add_signed(-d); // the frame clamps it
        return true;
    }
    let Some(pane) = (0..2).find(|&k| app.gh.hits.lists[k].contains(p)) else { return false };
    app.gh.off[pane] = app.gh.off[pane].saturating_add_signed(d); // the frame clamps it
    true
}

// ---------------------------------------------------------------- drawing

fn ic(i: usize) -> &'static str {
    icons().gh[i]
}

/// A button, lit under the pointer.
fn button(app: &mut App, f: &mut Frame, at: Rect, label: &str, primary: bool, b: Btn) -> u16 {
    let w = (label.width() as u16).min(at.width);
    let r = Rect::new(at.x, at.y, w, 1);
    let hot = app.hover.is_some_and(|p| r.contains(p));
    f.render_widget(Span::styled(fit(label, w as usize), crate::ui::button_style(primary, hot)), r);
    app.gh.hits.btns.push((r, b));
    w
}

/// A message (with an optional button under it) in the view.
fn message(app: &mut App, f: &mut Frame, area: Rect, lines: Vec<Line<'static>>, btn: Option<(&str, Btn)>) {
    let p = Paragraph::new(lines).wrap(Wrap { trim: true }).style(Style::new().fg(P.sidebar_fg));
    let h = p.line_count(area.width) as u16;
    f.render_widget(p, area);
    if let Some((label, b)) = btn {
        let y = area.y + h + 1;
        if y < area.bottom() {
            button(app, f, Rect::new(area.x, y, area.width, 1), label, true, b);
        }
    }
}

/// The view below its title: what needs doing first (gh, signing in), else the two panes.
pub fn draw(app: &mut App, f: &mut Frame, r: Rect) {
    app.gh.hits.area = r;
    let focused = app.focus == Focus::Side && !app.activity_lit;
    let card = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, focused);
    let pad = (app.settings.spacing.sidebar_padding.saturating_sub(1) / 2).max(1);
    let area = inset(card, pad);
    if area.width < 8 || area.height < 3 {
        return;
    }
    app.ui.side_list = Rect::default();
    let dim = Style::new().fg(P.dim);
    match app.gh.state.clone() {
        State::Loading if app.gh.data.is_none() => message(app, f, area, vec![Line::styled("Loading…", dim)], None),
        State::NoGh => {
            let lines = vec![
                Line::styled("GitHub CLI (gh) is not installed.", Style::new().bold()),
                Line::raw(""),
                Line::raw("termcode reaches GitHub through it, so it never holds a password or token."),
                Line::raw(""),
                Line::styled("Install it from https://cli.github.com (Debian, Ubuntu: sudo apt install gh), then:", dim),
            ];
            message(app, f, area, lines, Some((" Try Again ", Btn::Retry)));
        }
        State::SignedOut => {
            let mut lines = vec![
                Line::styled("Sign in to GitHub", Style::new().bold()),
                Line::raw(""),
                Line::raw("to see your repositories, pull requests, review requests, issues and notifications."),
                Line::raw(""),
            ];
            if app.gh.signing_in {
                lines.push(Line::styled("Finish signing in in the terminal below: it shows a code to enter in your browser.", Style::new().fg(P.git_modified)));
                message(app, f, area, lines, None);
            } else {
                lines.push(Line::styled("gh asks for your account in the terminal below; termcode never sees a password or token.", dim));
                message(app, f, area, lines, Some((" Sign in with GitHub ", Btn::SignIn)));
            }
        }
        State::Failed(e) if app.gh.data.is_none() => {
            let lines = vec![Line::styled("Couldn't reach GitHub.", Style::new().bold()), Line::raw(""), Line::styled(e, Style::new().fg(P.error_fg))];
            message(app, f, area, lines, Some((" Try Again ", Btn::Retry)));
        }
        _ => ready(app, f, area, focused),
    }
}

/// The signed-in view: the account, the top pane, the line between, the bottom pane.
fn ready(app: &mut App, f: &mut Frame, area: Rect, focused: bool) {
    let Some(d) = app.gh.data.clone() else { return };
    let dim = Style::new().fg(P.dim);
    // The account: its round picture (two rows) and name; Refresh at the right.
    let pic = Rect::new(area.x, area.y, 4.min(area.width), 2.min(area.height));
    let drew = match (&app.gh.avatar, crate::theme::ascii()) {
        (Some((login, img)), false) if *login == d.login => {
            let g = app.gh.graphics.get_or_insert_with(crate::settingsgraphics::Graphics::new);
            g.picture(f, pic, img, P.sidebar_bg, P.icon_fg)
        }
        _ => false,
    };
    if !drew {
        f.render_widget(Span::styled(ic(GITHUB), Style::new().fg(P.sidebar_fg)), Rect::new(pic.x + 1, pic.y, 2, 1));
    }
    let tx = pic.right() + 1;
    let refresh = format!(" {} ", if app.gh.loading || app.gh.repo_loading { ic(RUNNING) } else { ic(REFRESH) });
    let rw = refresh.width() as u16;
    let text_w = area.right().saturating_sub(tx + rw + 1);
    f.render_widget(Span::styled(fit(&d.login, text_w as usize), Style::new().fg(P.sidebar_fg).bold()), Rect::new(tx, area.y, text_w, 1));
    let second = match &app.gh.state {
        State::Failed(e) => (format!("Couldn't refresh: {e}"), Style::new().fg(P.error_fg)),
        _ => (format!("{} repositories", d.repos.len()), dim),
    };
    if area.height > 1 {
        f.render_widget(Span::styled(fit(&second.0, (area.right() - tx) as usize), second.1), Rect::new(tx, area.y + 1, area.right() - tx, 1));
    }
    button(app, f, Rect::new(area.right() - rw, area.y, rw, 1), &refresh, false, Btn::Refresh);
    // The panes share what is left: the top one's height dragged (half by default), each
    // keeping a few rows.
    let y0 = area.y + 3;
    let body = area.bottom().saturating_sub(y0);
    if body < 6 {
        return;
    }
    // Half each by default: both have a graph.
    let top_h = app.gh.split.unwrap_or(body / 2).clamp(3, body - 4);
    let top = Rect::new(area.x, y0, area.width, top_h);
    let sash = Rect::new(area.x, y0 + top_h, area.width, 1);
    let bottom = Rect::new(area.x, sash.y + 1, area.width, area.bottom() - sash.y - 1);
    app.gh.hits.sash = sash;
    app.gh.hits.panes = [top, bottom];
    let top = activity(app, f, top, &d);
    list(app, f, top, TOP, focused);
    // The line between: the chosen repository's name in it, lit while dragged or hovered.
    let hot = matches!(app.drag, Drag::GhSash) || app.hover.is_some_and(|p| sash.contains(p));
    let line_fg = if hot { P.sash_hover } else { P.group_border };
    let name = app.gh.chosen.as_deref().map(|n| format!(" {} {} ", ic(REPO), n)).unwrap_or_default();
    let h = if crate::theme::ascii() { "-" } else { "─" };
    let label_w = (name.width() as u16).min(sash.width.saturating_sub(2));
    row(f, sash, P.sidebar_bg, vec![
        Span::styled(h.repeat(2), Style::new().fg(line_fg)),
        Span::styled(fit(&name, label_w as usize), Style::new().fg(P.sidebar_title).bold()),
        Span::styled(h.repeat(sash.width.saturating_sub(2 + label_w) as usize), Style::new().fg(line_fg)),
    ]);
    repo_pane(app, f, bottom, focused);
}

/// `a` blended `t` of the way to `b` (theme colours, opaque).
fn mix(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(m(r1, r2), m(g1, g2), m(b1, b2))
        }
        _ => b,
    }
}

/// A contribution level's colour: a faint block for none, then four greens.
fn level_color(level: u8) -> Color {
    match level {
        0 => mix(P.sidebar_bg, P.sidebar_fg, 0.12),
        l => mix(P.sidebar_bg, P.gh_green, [0.3, 0.55, 0.8, 1.0][(l as usize - 1).min(3)]),
    }
}

/// Cells a week takes in the year graph, and a day in the month's: the blocks' size, the
/// same in both.
const YEAR_PITCH: u16 = 2;
const MONTH_PITCH: u16 = YEAR_PITCH;

/// A day's block in its level's colour (`#` in ASCII).
fn block(level: u8) -> Span<'static> {
    let glyph = if crate::theme::ascii() { "#" } else { "■" };
    Span::styled(glyph, Style::new().fg(level_color(level)))
}

/// Today, as the calendar writes a date ("2026-11-13", UTC as GitHub's).
fn today() -> String {
    let (y, m, d) = ymd(crate::scm::now());
    format!("{y}-{m:02}-{d:02}")
}

/// "Nov 8" for "2026-11-08".
fn short_date(date: &str) -> String {
    let m: usize = date.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1).clamp(1, 12);
    let d: u32 = date.get(8..10).and_then(|d| d.parse().ok()).unwrap_or(0);
    format!("{} {d}", MONTHS[m - 1])
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// (year, month 1-12, day) of unix time `t` (UTC).
pub fn ymd(t: i64) -> (i32, u32, u32) {
    // Howard Hinnant's civil_from_days.
    let z = t.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((yoe + era * 400 + i64::from(m <= 2)) as i32, m, d)
}

/// A calendar year's contributions, fetched once (the month view and an older year).
fn want_year(app: &mut App, y: i32) {
    if app.gh.years.contains_key(&y) || app.gh.year_asked.is_some() {
        return;
    }
    app.gh.year_asked = Some(y);
    let (bin, tx, root) = (app.gh.bin.clone(), app.gh.tx.clone(), cwd(app));
    let query = ACTIVITY.replace("contributionsCollection", &format!("contributionsCollection(from: \"{y}-01-01T00:00:00Z\", to: \"{y}-12-31T23:59:59Z\")"));
    std::thread::spawn(move || {
        let a = gh_json(&bin, &root, &["api", "graphql", "-f", &query]).ok().and_then(|v| activity_of(&v));
        let _ = tx.send(Msg::Year(y, a));
    });
}

/// ‹ or ›: an older (`d` -1) or newer year (the last 12 months newest), or month, no
/// further back than the account and on to this month.
fn act_step(app: &mut App, d: i32) {
    let (now_y, now_m, _) = ymd(crate::scm::now());
    let since = app.gh.data.as_ref().map_or(now_y, |x| x.since.min(now_y));
    let gh = &mut app.gh;
    gh.act_scroll[ACCOUNT] = 0;
    if gh.act_month {
        let (y, m) = gh.act_ym;
        let i = (y * 12 + m as i32 - 1 + d).clamp(since * 12, now_y * 12 + now_m as i32 - 1);
        gh.act_ym = (i.div_euclid(12), (i.rem_euclid(12) + 1) as u32);
    } else {
        // Newest first: the last 12 months, then each year back.
        let at = gh.act_year.map_or(0, |y| now_y - y + 1);
        let at = (at - d).clamp(0, now_y - since + 1);
        gh.act_year = (at > 0).then(|| now_y - at + 1);
    }
}

/// The calendar the graph shows now: the last 12 months, or a fetched year (asked for
/// when missing: None meanwhile).
fn calendar(app: &mut App) -> Option<Activity> {
    let year = if app.gh.act_month { Some(app.gh.act_ym.0) } else { app.gh.act_year };
    match year {
        None => app.gh.data.as_ref()?.activity.clone(),
        Some(y) => {
            want_year(app, y);
            app.gh.years.get(&y).cloned()
        }
    }
}

/// The account's activity at the top of the upper pane, as GitHub draws it: a header
/// (which folds it) with the contributions shown, or the day under the pointer; Year and
/// Month with ‹ › to go back; a block a day in green; the legend and the account's
/// numbers. Returns what is left for the list (which keeps a few rows, or the graph does
/// not show).
fn activity(app: &mut App, f: &mut Frame, r: Rect, d: &Data) -> Rect {
    if d.activity.is_none() {
        return r;
    }
    let folded = app.gh.activity_folded;
    let month = app.gh.act_month;
    // The grid's rows: text blocks a row a weekday (a week of the month); pixels as tall
    // as their squares (a cell wide a day in the year, two in the month).
    let px = pixels(app);
    let grid = match (month, px) {
        (false, Some((cw, ch))) => (7 * u32::from(YEAR_PITCH * cw)).div_ceil(u32::from(ch)) as u16,
        (false, None) => 7,
        (true, Some((cw, ch))) => (6 * u32::from(MONTH_PITCH * cw)).div_ceil(u32::from(ch)) as u16,
        (true, None) => 6,
    };
    // Header and Year | Month, the labels and the grid, the legend, the numbers, a gap. The
    // list below keeps three rows; short of the whole graph, its header alone shows.
    if r.height < 2 + 3 || r.width < 18 {
        return r;
    }
    let folded = folded || r.height < 2 + 1 + grid + 3 + 3;
    let used = if folded { 2 } else { 2 + 1 + grid + 3 };
    let cal = calendar(app);
    let (when, total) = match (month, app.gh.act_year, &cal) {
        (true, ..) => {
            let (y, m) = app.gh.act_ym;
            let key = format!("{y}-{m:02}");
            let n: u64 = cal.iter().flat_map(|c| c.weeks.iter().flatten()).filter(|x| x.date.starts_with(&key)).map(|x| x.count).sum();
            (format!("{} {y}", MONTHS[m as usize - 1]), n)
        }
        (false, None, c) => ("the past year".to_string(), c.as_ref().map_or(0, |c| c.total)),
        (false, Some(y), c) => (y.to_string(), c.as_ref().map_or(0, |c| c.total)),
    };
    // The header (drawn last: it names the block under the pointer).
    let chev = if folded { icons().chev_right } else { icons().chev_down };
    let hr = Rect::new(r.x, r.y, r.width, 1);
    let header = |app: &App, f: &mut Frame| {
        let hovered = app.hover.and_then(|p| app.gh.hits.act_cells.iter().find(|(c, .., w)| *w == ACCOUNT && c.contains(p)));
        let what = match hovered {
            Some((_, date, n, _)) => format!("{}: {n} contribution{}", short_date(date), if *n == 1 { "" } else { "s" }),
            None => format!("{total} in {when}"),
        };
        row(f, hr, P.section_bg, vec![Span::styled(fit(&format!("{chev} ACTIVITY · {what}"), r.width as usize), Style::new().fg(P.section_fg).bold())]);
    };
    app.gh.hits.btns.push((hr, Btn::Activity));
    if folded {
        header(app, f);
        return Rect::new(r.x, r.y + 2, r.width, r.height - 2);
    }
    // Year | Month at the left, ‹ what › at the right.
    let cy = r.y + 1;
    let mut x = r.x;
    for (label, m) in [(" Year ", false), (" Month ", true)] {
        x += button(app, f, Rect::new(x, cy, r.right() - x, 1), label, month == m, Btn::ActMode(m)) + 1;
    }
    let step = format!(" {} ", when.strip_prefix("the ").unwrap_or(&when));
    let sx = r.right().saturating_sub(step.width() as u16 + 4);
    if sx > x {
        button(app, f, Rect::new(sx, cy, 2, 1), "‹ ", false, Btn::ActStep(-1));
        f.render_widget(Span::styled(step.clone(), Style::new().fg(P.sidebar_fg)), Rect::new(sx + 2, cy, step.width() as u16, 1));
        button(app, f, Rect::new(r.right() - 2, cy, 2, 1), " ›", false, Btn::ActStep(1));
    }
    let g = Rect::new(r.x, r.y + 2, r.width, 1 + grid);
    let (start, end) = match &cal {
        None => {
            f.render_widget(Span::styled("Loading…", Style::new().fg(P.dim)), Rect::new(g.x + 2, g.y + 2, g.width.saturating_sub(2), 1));
            (r.x, r.right())
        }
        Some(c) if month => month_view(app, f, g, c, px),
        Some(c) => year_view(app, f, g, c, px, ACCOUNT),
    };
    header(app, f);
    // Less ■■■■■ More centred under the graph, then the account's numbers.
    let ly = r.y + used - 3;
    let mut legend = vec![Span::styled("Less ", Style::new().fg(P.dim))];
    legend.extend((0..5).flat_map(|l| [block(l), Span::raw(" ")]));
    legend.push(Span::styled("More", Style::new().fg(P.dim)));
    let lw = legend.iter().map(|s| s.width() as u16).sum::<u16>();
    let lx = ((start + end) / 2).saturating_sub(lw / 2).clamp(r.x, r.right().saturating_sub(lw));
    f.render_widget(Line::from(legend), Rect::new(lx, ly, lw.min(r.width), 1));
    let earned: u64 = d.repos.iter().map(|r| r.stars).sum();
    let a = d.activity.as_ref().expect("checked above");
    let s = |n: u64| if n == 1 { "" } else { "s" };
    let numbers = format!("{} repos · {} {earned} · {} follower{} · {} following", d.repos.len(), ic(STAR), a.followers, s(a.followers), a.following);
    f.render_widget(Span::styled(fit(&numbers, r.width as usize), Style::new().fg(P.dim)), Rect::new(r.x, ly + 1, r.width, 1));
    Rect::new(r.x, r.y + used, r.width, r.height - used)
}

/// Pixels for the graphs (Kitty graphics, not ASCII): a cell's size; None for text.
fn pixels(app: &mut App) -> Option<(u16, u16)> {
    let g = app.gh.graphics.get_or_insert_with(crate::settingsgraphics::Graphics::new);
    (g.pixel_controls() && !crate::theme::ascii()).then(|| g.cell())
}

/// The graph as pixels: each day a square (rounded off at its corners) `pitch` px apart at
/// its (column, row), a gap round it, on `bg`.
pub fn grid_image(w: u32, h: u32, pitch: u32, days: &[(u32, u32, Color)], bg: Color) -> RgbaImage {
    use crate::settingsgraphics::pixel;
    let gap = (pitch / 5).max(1);
    let side = pitch.saturating_sub(gap).max(1);
    let mut img = RgbaImage::from_pixel(w, h, pixel(bg));
    for &(col, row, color) in days {
        let (x0, y0) = (col * pitch + gap / 2, row * pitch + gap / 2);
        for y in y0..(y0 + side).min(h) {
            for x in x0..(x0 + side).min(w) {
                let corner = side >= 6 && (x == x0 || x == x0 + side - 1) && (y == y0 || y == y0 + side - 1);
                if !corner {
                    img.put_pixel(x, y, pixel(color));
                }
            }
        }
    }
    img
}

/// A digest of a graph's squares and colours: its image is made again only when it changes.
fn digest(days: &[(u32, u32, Color)], size: (u16, u16)) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    size.hash(&mut h);
    format!("{days:?}{:?}", (P.gh_green, P.sidebar_bg, P.sidebar_fg)).hash(&mut h);
    h.finish()
}

/// GitHub's year graph in `g`: month names over a column a week, a row a weekday (Sunday
/// first; Mon, Wed and Fri named), newest week at the right; the wheel scrolls it back. A
/// week takes YEAR_PITCH cells: with pixels (`px`) a square that wide, else a block.
/// Returns the graph's left and right edges.
fn year_view(app: &mut App, f: &mut Frame, g: Rect, c: &Activity, px: Option<(u16, u16)>, which: usize) -> (u16, u16) {
    let cols = (g.width.saturating_sub(2) / YEAR_PITCH) as usize;
    let n = c.weeks.len();
    let shown = cols.min(n);
    app.gh.act_scroll[which] = app.gh.act_scroll[which].min(n - shown);
    let start = n - shown - app.gh.act_scroll[which];
    // Centred: the weekday names, then the weeks.
    let g = Rect { x: g.x + (g.width - (2 + shown as u16 * YEAR_PITCH)) / 2, ..g };
    let gx = g.x + 2;
    // A weekday's text row: its own, or (pixels) where its square's middle falls.
    let row_of = |wd: u32| match px {
        Some((cw, ch)) => {
            let pitch = u32::from(YEAR_PITCH * cw);
            g.y + 1 + ((wd * pitch + pitch / 2) / u32::from(ch)) as u16
        }
        None => g.y + 1 + wd as u16,
    };
    let mut taken = Vec::new();
    for (k, label) in [(1, "M"), (3, "W"), (5, "F")] {
        let y = row_of(k);
        if !taken.contains(&y) && y < g.bottom() {
            taken.push(y);
            f.render_widget(Span::styled(label, Style::new().fg(P.dim)), Rect::new(g.x, y, 1, 1));
        }
    }
    let mut squares = Vec::new();
    let mut free = gx; // month names never overlap
    for (i, w) in c.weeks[start..start + shown].iter().enumerate() {
        let x = gx + i as u16 * YEAR_PITCH;
        // A month's name over the week its first day falls in.
        if let Some(first) = w.iter().find(|d| d.date.ends_with("-01")) {
            let m: usize = first.date.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1);
            if x >= free && x + 3 <= g.right() {
                f.render_widget(Span::styled(MONTHS[m - 1], Style::new().fg(P.dim)), Rect::new(x, g.y, 3, 1));
                free = x + 4;
            }
        }
        for day in w.iter().filter(|d| d.date <= today()) {
            let cell = Rect::new(x, row_of(u32::from(day.weekday)), YEAR_PITCH, 1);
            if px.is_some() {
                squares.push((i as u32, u32::from(day.weekday), level_color(day.level)));
            } else {
                f.render_widget(block(day.level), Rect::new(x, cell.y, 1, 1));
            }
            app.gh.hits.act_cells.push((cell, day.date.clone(), day.count, which));
        }
    }
    let area = Rect::new(gx, g.y + 1, shown as u16 * YEAR_PITCH, g.height - 1);
    if let (Some((cw, _)), Some(gr)) = (px, app.gh.graphics.as_mut()) {
        let key = digest(&squares, (area.width, area.height));
        // Slot 7 the account's, 9 the repository's.
        gr.raw(f, area, 7 + 2 * which, key, |w, h| grid_image(w, h, u32::from(YEAR_PITCH * cw), &squares, P.sidebar_bg));
    }
    app.gh.hits.act_graph[which] = area;
    (g.x, area.right())
}

/// One month as a calendar in `g`: weekday initials over a block a day, MONTH_PITCH cells
/// apart (with pixels, squares that wide).
/// Returns the graph's left and right edges.
fn month_view(app: &mut App, f: &mut Frame, g: Rect, c: &Activity, px: Option<(u16, u16)>) -> (u16, u16) {
    let (y, m) = app.gh.act_ym;
    let key = format!("{y}-{m:02}-");
    let days: Vec<&Day> = c.weeks.iter().flatten().filter(|d| d.date.starts_with(&key)).collect();
    let today = today();
    // Centred: seven days across.
    let width = (7 * MONTH_PITCH).min(g.width);
    let gx = g.x + (g.width - width) / 2;
    let step = MONTH_PITCH;
    for (k, label) in ["S", "M", "T", "W", "T", "F", "S"].iter().enumerate() {
        f.render_widget(Span::styled(*label, Style::new().fg(P.dim)), Rect::new(gx + step * k as u16, g.y, 1, 1));
    }
    let Some(first) = days.first() else {
        let note = "No contributions this month.";
        let x = g.x + g.width.saturating_sub(note.width() as u16) / 2;
        f.render_widget(Span::styled(note, Style::new().fg(P.dim)), Rect::new(x, g.y + 2, g.right() - x, 1));
        return (g.x, g.right());
    };
    let lead = first.weekday as usize;
    let mut squares = Vec::new();
    for (i, day) in days.iter().enumerate() {
        let slot = lead + i;
        let (col, week) = ((slot % 7) as u32, (slot / 7) as u32);
        let cell = match px {
            Some((cw, ch)) => {
                let pitch = u32::from(MONTH_PITCH * cw);
                Rect::new(gx + MONTH_PITCH * col as u16, g.y + 1 + ((week * pitch + pitch / 2) / u32::from(ch)) as u16, MONTH_PITCH, 1)
            }
            None => Rect::new(gx + MONTH_PITCH * col as u16, g.y + 1 + week as u16, 1, 1),
        };
        // Days to come have no block, as on GitHub.
        if cell.bottom() > g.bottom() || day.date > today {
            break;
        }
        if px.is_some() {
            squares.push((col, week, level_color(day.level)));
        } else {
            f.render_widget(block(day.level), cell);
        }
        app.gh.hits.act_cells.push((cell, day.date.clone(), day.count, ACCOUNT));
    }
    if let (Some((cw, _)), Some(gr)) = (px, app.gh.graphics.as_mut()) {
        let area = Rect::new(gx, g.y + 1, width, g.height - 1);
        let key = digest(&squares, (area.width, area.height));
        gr.raw(f, area, 8, key, |w, h| grid_image(w, h, u32::from(MONTH_PITCH * cw), &squares, P.sidebar_bg));
    }
    app.gh.hits.act_graph[ACCOUNT] = Rect::default();
    (gx, gx + width)
}

/// The bottom pane: the chosen repository's description and status, then its lists.
fn repo_pane(app: &mut App, f: &mut Frame, r: Rect, focused: bool) {
    let dim = Style::new().fg(P.dim);
    let Some(name) = app.gh.chosen.clone() else {
        let lines = vec![Line::styled("Choose a repository above to see its Actions, pull requests and issues here.", dim)];
        return message(app, f, r, lines, None);
    };
    let info = app.gh.chosen_info().cloned().unwrap_or_else(|| RepoInfo { name: name.clone(), ..RepoInfo::default() });
    // Its description, then what it is: private, default branch, language, stars, forks,
    // last push; its page at the right.
    let now = crate::scm::now();
    let ow = 3;
    button(app, f, Rect::new(r.right().saturating_sub(ow), r.y, ow, 1), &format!(" {} ", ic(OPEN)), false, Btn::OpenRepo);
    let desc = if info.description.is_empty() { "No description.".to_string() } else { info.description.clone() };
    f.render_widget(Span::styled(fit(&desc, r.width.saturating_sub(ow + 1) as usize), Style::new().fg(P.sidebar_fg)), Rect::new(r.x, r.y, r.width.saturating_sub(ow + 1), 1));
    let mut facts: Vec<String> = Vec::new();
    if info.private {
        facts.push(format!("{} private", icons().lock));
    }
    if info.archived {
        facts.push("archived".into());
    }
    if info.fork {
        facts.push("fork".into());
    }
    facts.extend([info.branch.clone(), info.language.clone()].into_iter().filter(|s| !s.is_empty()));
    facts.push(format!("{} {}", ic(STAR), info.stars));
    facts.push(format!("{} {}", ic(FORK), info.forks));
    if info.pushed > 0 {
        facts.push(format!("pushed {}", crate::scm::ago_short(info.pushed, now)));
    }
    if r.height > 1 {
        f.render_widget(Span::styled(fit(&facts.join(" · "), r.width as usize), dim), Rect::new(r.x, r.y + 1, r.width, 1));
    }
    let lists = Rect::new(r.x, r.y + 3, r.width, r.height.saturating_sub(3));
    match &app.gh.repo {
        Some(Err(e)) => {
            let lines = vec![Line::styled(format!("Couldn't read it: {e}"), Style::new().fg(P.error_fg))];
            message(app, f, lists, lines, Some((" Try Again ", Btn::Retry)));
        }
        None => message(app, f, lists, vec![Line::styled("Loading…", dim)], None),
        Some(Ok(_)) => {
            let lists = stats(app, f, lists);
            list(app, f, lists, BOTTOM, focused)
        }
    }
}

/// Rows the repository's lists keep under its stats (a header and three).
pub const LIST_ROWS: u16 = 4;

/// The repository's numbers above its lists: a header (which folds them) with its commits in
/// the past year, or the day under the pointer; its commit graph, as the account's; its
/// languages as a bar in their colours with their shares; its top contributors. Returns
/// what is left for the lists (which keep a few rows, or the stats do not show).
fn stats(app: &mut App, f: &mut Frame, r: Rect) -> Rect {
    let Some(Ok(rd)) = app.gh.repo.clone() else { return r };
    let folded = app.gh.stats_folded;
    let px = pixels(app);
    let grid = match px {
        Some((cw, ch)) => (7 * u32::from(YEAR_PITCH * cw)).div_ceil(u32::from(ch)) as u16,
        None => 7,
    };
    // The lists below keep LIST_ROWS rows. Of the rest: the header always, then by turns
    // what fits: the graph, the language bar, the contributors, the key.
    let mut room = r.height.saturating_sub(LIST_ROWS);
    if room == 0 || r.width < 18 {
        return r;
    }
    room -= 1;
    let mut take = |want: bool, rows: u16| {
        let fits = want && !folded && room >= rows;
        if fits {
            room -= rows;
        }
        fits
    };
    let show_graph = take(true, 1 + grid);
    let show_bar = take(!rd.languages.is_empty(), 1);
    let show_people = take(!rd.contributors.is_empty(), 1);
    let show_key = take(show_bar, 1);
    let used = if folded {
        2
    } else {
        1 + if show_graph { 1 + grid } else { 0 } + u16::from(show_bar) + u16::from(show_people) + u16::from(show_key)
    };
    let dim = Style::new().fg(P.dim);
    let g = Rect::new(r.x, r.y + 1, r.width, if show_graph { 1 + grid } else { 0 });
    if show_graph {
        match &rd.commits {
            Some(c) => {
                year_view(app, f, g, c, px, REPO_GRAPH);
            }
            None => {
                let note = "GitHub is counting its commits…";
                let x = g.x + g.width.saturating_sub(note.width() as u16) / 2;
                f.render_widget(Span::styled(note, dim), Rect::new(x, g.y + 1 + grid / 2, g.right() - x, 1));
            }
        }
    }
    // The header last: it names the day under the pointer.
    let hovered = app.hover.and_then(|p| app.gh.hits.act_cells.iter().find(|(c, .., w)| *w == REPO_GRAPH && c.contains(p)));
    let what = match (hovered, &rd.commits) {
        (Some((_, date, n, _)), _) => format!("{}: {n} commit{}", short_date(date), if *n == 1 { "" } else { "s" }),
        (None, Some(c)) => format!("{} commits in the past year", c.total),
        (None, None) => "counting…".to_string(),
    };
    let chev = if folded { icons().chev_right } else { icons().chev_down };
    let hr = Rect::new(r.x, r.y, r.width, 1);
    row(f, hr, P.section_bg, vec![Span::styled(fit(&format!("{chev} STATS · {what}"), r.width as usize), Style::new().fg(P.section_fg).bold())]);
    app.gh.hits.btns.push((hr, Btn::Stats));
    if folded {
        return Rect::new(r.x, r.y + 2, r.width, r.height - 2);
    }
    let mut y = g.bottom();
    if show_bar {
        // The bar: a cell per share (at least one), the last language to the end.
        let total: u64 = rd.languages.iter().map(|l| l.1).sum::<u64>().max(1);
        let ascii = crate::theme::ascii();
        let mut x = r.x;
        for (i, (lang, bytes)) in rd.languages.iter().enumerate() {
            if x >= r.right() {
                break;
            }
            let cells = if i + 1 == rd.languages.len() { r.right() - x } else { ((bytes * u64::from(r.width) + total / 2) / total).max(1).min(u64::from(r.right() - x)) as u16 };
            let glyph = if ascii { "#" } else { "█" };
            f.render_widget(Span::styled(glyph.repeat(cells as usize), Style::new().fg(lang_color(lang))), Rect::new(x, y, cells, 1));
            x += cells;
        }
        let dot = if ascii { "*" } else { "●" };
        let mut key: Vec<Span> = Vec::new();
        for (lang, bytes) in &rd.languages {
            let share = *bytes as f64 * 100.0 / total as f64;
            key.push(Span::styled(dot, Style::new().fg(lang_color(lang))));
            key.push(Span::styled(format!(" {lang} {share:.1}%  "), dim));
        }
        if show_key {
            f.render_widget(Line::from(key), Rect::new(r.x, y + 1, r.width, 1));
            y += 1;
        }
        y += 1;
    }
    if show_people {
        let who = rd.contributors.iter().map(|(n, c)| format!("{n} {c}")).collect::<Vec<_>>().join(" · ");
        f.render_widget(Span::styled(fit(&format!("Contributors: {who}"), r.width as usize), dim), Rect::new(r.x, y, r.width, 1));
    }
    Rect::new(r.x, r.y + used, r.width, r.height - used)
}

/// A pane's list, scrolled to keep its selection in view.
fn list(app: &mut App, f: &mut Frame, r: Rect, pane: usize, focused: bool) {
    app.gh.hits.lists[pane] = r;
    let all = rows(&app.gh, pane);
    let h = r.height as usize;
    if all.is_empty() || h == 0 {
        return;
    }
    let keys = focused && app.gh.pane == pane;
    let gh = &mut app.gh;
    gh.sel[pane] = gh.sel[pane].min(all.len() - 1);
    if std::mem::take(&mut gh.follow[pane]) {
        if gh.sel[pane] < gh.off[pane] {
            gh.off[pane] = gh.sel[pane];
        } else if gh.sel[pane] >= gh.off[pane] + h {
            gh.off[pane] = gh.sel[pane] + 1 - h;
        }
    }
    gh.off[pane] = gh.off[pane].min(all.len().saturating_sub(h));
    let (off, sel) = (gh.off[pane], gh.sel[pane]);
    let now = crate::scm::now();
    // A scrolling list keeps its last column for the scrollbar.
    let w = r.width.saturating_sub(u16::from(all.len() > h));
    for (i, &rw) in all.iter().enumerate().skip(off).take(h) {
        let rr = Rect::new(r.x, r.y + (i - off) as u16, w, 1);
        app.gh.hits.rows.push((rr, pane, i));
        let hot = app.hover.is_some_and(|p| rr.contains(p));
        if let Row::Head(s) = rw {
            head(app, f, rr, s, i == sel && keys);
            continue;
        }
        let bg = if i == sel && keys {
            P.list_active_bg
        } else if i == sel {
            P.list_inactive_bg
        } else if hot {
            P.list_hover_bg
        } else {
            P.sidebar_bg
        };
        item(app, f, rr, bg, (pane, i, rw), now, hot || i == sel);
    }
    if all.len() > h {
        let track = Rect::new(r.right().saturating_sub(1), r.y, 1, r.height);
        let s = crate::decor::slider(track.height, all.len(), h, off, all.len() - h);
        for y in track.y + s.0..(track.y + s.0 + s.1).min(track.bottom()) {
            f.buffer_mut()[(track.x, y)].set_symbol("▐").set_fg(P.dim);
        }
    }
}

/// A section's header: its chevron, name and count, in the section colours.
fn head(app: &App, f: &mut Frame, r: Rect, s: Sec, focused: bool) {
    let gh = &app.gh;
    let chev = if gh.folded[s as usize] { icons().chev_right } else { icons().chev_down };
    let d = gh.data.as_ref();
    let (name, count) = match s {
        Sec::Repos => ("REPOSITORIES", d.map_or(0, |d| d.repos.len())),
        Sec::Reviews => ("REVIEW REQUESTS", gh.prs(s).len()),
        Sec::Mine => ("MY PULL REQUESTS", gh.prs(s).len()),
        Sec::Assigned => ("ASSIGNED TO ME", gh.issues(s).len()),
        Sec::Notes => ("NOTIFICATIONS", d.map_or(0, |d| d.notes.len())),
        Sec::Runs => ("ACTIONS", gh.runs().len()),
        Sec::RepoPrs => ("PULL REQUESTS", gh.prs(s).len()),
        Sec::RepoIssues => ("ISSUES", gh.issues(s).len()),
    };
    let fg = if focused { P.focus_border } else { P.section_fg };
    let count = format!(" {count} ");
    let name = fit(&format!("{chev} {name}"), (r.width as usize).saturating_sub(count.width()));
    row(f, r, P.section_bg, vec![Span::styled(name, Style::new().fg(fg).bold())]);
    crate::ui::right(f, r, vec![Span::styled(count, Style::new().fg(P.dim).bg(P.section_bg))]);
}

/// A repository's name as the account sees it: its own without the owner.
fn repo_label(gh: &GitHub, name: &str) -> String {
    let login = gh.data.as_ref().map_or("", |d| d.login.as_str());
    match name.split_once('/') {
        Some((owner, n)) if owner == login => n.to_string(),
        _ => name.to_string(),
    }
}

/// A row: its icon and title, then who (or which repository) and when; its buttons
/// instead while selected or under the pointer.
fn item(app: &mut App, f: &mut Frame, r: Rect, bg: Color, (pane, i, rw): (usize, usize, Row), now: i64, buttons: bool) {
    let gh = &app.gh;
    let green = P.gh_green;
    let ago = |t: i64| crate::scm::ago_short(t, now);
    let (icon, color, label, meta) = match rw {
        Row::Repo(k) => {
            let Some(rp) = gh.data.as_ref().map(|d| &d.repos[k]) else { return };
            // The chosen one's icon is lit.
            let color = if gh.chosen.as_deref() == Some(rp.name.as_str()) { P.focus_border } else { P.icon_fg };
            let lock = if rp.private { format!(" {}", icons().lock) } else { String::new() };
            (ic(REPO), color, format!("{}{lock}", repo_label(gh, &rp.name)), ago(rp.pushed))
        }
        Row::Pr(s, k) => {
            let p = &gh.prs(s)[k];
            let (icon, color) = if p.draft { (ic(PR_DRAFT), P.dim) } else { (ic(PR), green) };
            let checks = match p.checks {
                Checks::Pass => format!("{} ", ic(PASS)),
                Checks::Fail => format!("{} ", ic(FAIL)),
                Checks::Pending => format!("{} ", ic(RUNNING)),
                Checks::None => String::new(),
            };
            // The account's lists span repositories: they say which.
            let who = if s == Sec::RepoPrs { p.author.clone() } else { repo_label(gh, &p.repo) };
            (icon, color, format!("#{} {}", p.number, p.title), format!("{checks}{who} · {}", ago(p.updated)))
        }
        Row::Issue(s, k) => {
            let is = &gh.issues(s)[k];
            let who = if s == Sec::RepoIssues { is.author.clone() } else { repo_label(gh, &is.repo) };
            (ic(ISSUE), green, format!("#{} {}", is.number, is.title), format!("{who} · {}", ago(is.updated)))
        }
        Row::Run(k) => {
            let rn = &gh.runs()[k];
            let (icon, color) = run_icon(rn.state);
            (icon, color, rn.title.clone(), format!("{} · {}", rn.workflow, ago(rn.updated)))
        }
        Row::Note(k) => {
            let Some(n) = gh.data.as_ref().map(|d| &d.notes[k]) else { return };
            let icon = match n.kind.as_str() {
                "PullRequest" => ic(PR),
                "Issue" => ic(ISSUE),
                _ => ic(GITHUB),
            };
            (icon, P.sidebar_fg, n.title.clone(), format!("{} · {}", repo_label(gh, &n.repo), ago(n.updated)))
        }
        Row::Empty(s) => {
            let what = match s {
                Sec::Repos => "No repositories.",
                Sec::Reviews => "No pull requests wait for your review.",
                Sec::Mine => "You have no open pull requests.",
                Sec::Assigned => "No open issues are assigned to you.",
                Sec::Notes => "No unread notifications.",
                Sec::Runs => "No workflow runs.",
                Sec::RepoPrs => "No open pull requests.",
                Sec::RepoIssues => "No open issues.",
            };
            row(f, r, bg, vec![Span::styled(fit(&format!("  {what}"), r.width as usize), Style::new().fg(P.dim).italic())]);
            return;
        }
        Row::Head(_) => return,
    };
    // The tail (who and when, or the buttons) ends a cell before the edge; two cells part
    // it from the title, which is cut first.
    let keys = if buttons { keys_of(gh, rw) } else { Vec::new() };
    let lead = format!("  {icon} ");
    let tail_w = if keys.is_empty() { meta.width() as u16 } else { keys.len() as u16 * 2 }.min(r.width * 2 / 5);
    let title_w = r.width.saturating_sub(lead.width() as u16 + tail_w + 3) as usize;
    row(f, r, bg, vec![Span::styled(lead.clone(), Style::new().fg(color)), Span::styled(fit(&label, title_w), Style::new().fg(P.sidebar_fg))]);
    let tx = r.right().saturating_sub(tail_w + 1);
    if keys.is_empty() {
        f.render_widget(Span::styled(fit(&meta, tail_w as usize), Style::new().fg(P.dim).bg(bg)), Rect::new(tx, r.y, tail_w, 1));
    } else {
        for (k, key) in keys.iter().enumerate() {
            let glyph = match key {
                Key::Edit => ic(EDIT),
                Key::Checkout => icons().branch,
                Key::Close | Key::Cancel => ic(STOP),
                Key::Rerun => ic(RERUN),
                Key::Open => ic(OPEN),
            };
            let br = Rect::new(tx + k as u16 * 2, r.y, 2, 1).intersection(r);
            let hot = app.hover.is_some_and(|p| br.contains(p));
            let st = if hot { Style::new().fg(P.sidebar_fg).bg(P.button2_hover) } else { Style::new().fg(P.icon_fg).bg(bg) };
            f.render_widget(Span::styled(format!("{glyph} "), st), br);
            app.gh.hits.btns.push((br, Btn::Act(pane, i, *key)));
        }
    }
    // The title field over the row's title while it is edited.
    let gh = &app.gh;
    let (pr, repo, n) = match rw {
        Row::Pr(s, k) => (true, gh.prs(s)[k].repo.clone(), gh.prs(s)[k].number),
        Row::Issue(s, k) => (false, gh.issues(s)[k].repo.clone(), gh.issues(s)[k].number),
        _ => return,
    };
    if let Some(e) = gh.edit.as_ref().filter(|e| e.pr == pr && e.n == n && e.repo == repo) {
        let x = r.x + lead.width() as u16;
        let field = Rect::new(x, r.y, r.right().saturating_sub(x + 1), 1);
        let skip = crate::ui::rename_field(f, field, &e.field, false);
        app.gh.hits.field = (field, skip);
    }
}

fn run_icon(s: RunState) -> (&'static str, Color) {
    match s {
        RunState::Pass => (ic(PASS), P.gh_green),
        RunState::Fail => (ic(FAIL), P.error_fg),
        RunState::Running => (ic(RUNNING), P.gh_yellow),
        RunState::Queued => (ic(QUEUED), P.gh_yellow),
        RunState::Skipped => (ic(SKIPPED), P.dim),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_give_their_host_and_repository() {
        for (url, h, path) in [
            ("https://github.com/JonasAtelier/termcode.git", Some("github.com"), Some("JonasAtelier/termcode")),
            ("git@github.com:o/r.git", Some("github.com"), Some("o/r")),
            ("ssh://git@ssh.github.com:443/o/r", Some("ssh.github.com"), Some("o/r")),
            ("ssh://gerrit.example.com:29418/project/kernel", Some("gerrit.example.com"), Some("project/kernel")),
            ("https://user:pw@GitHub.example.com/o/r/", Some("github.example.com"), Some("o/r")),
            ("/srv/git/r.git", None, None),
        ] {
            assert_eq!((host(url).as_deref(), repo_path(url).as_deref().filter(|_| h.is_some())), (h, path), "{url}");
        }
    }

    #[test]
    fn the_pixel_graph_draws_a_rounded_square_a_day_with_a_gap() {
        use crate::settingsgraphics::pixel;
        let (bg, green) = (Color::Rgb(0, 0, 0), Color::Rgb(0, 200, 0));
        // Pitch 10: a 2px gap, squares 8px, from 1px in.
        let img = grid_image(30, 20, 10, &[(1, 0, green), (2, 1, green)], bg);
        assert_eq!(*img.get_pixel(15, 5), pixel(green), "inside week 1, Sunday");
        assert_eq!(*img.get_pixel(10, 5), pixel(bg), "the gap before it");
        assert_eq!(*img.get_pixel(11, 1), pixel(bg), "its corner, rounded off");
        assert_eq!(*img.get_pixel(12, 1), pixel(green));
        assert_eq!(*img.get_pixel(25, 15), pixel(green), "week 2, Monday");
        assert_eq!(*img.get_pixel(5, 5), pixel(bg), "no day: background");
    }

    #[test]
    fn repository_stats_come_from_their_endpoints() {
        assert_eq!(commits_of(&serde_json::json!({})), None, "GitHub still counting (202)");
        let c = commits_of(&serde_json::json!([{"days": [0, 1, 0, 0, 0, 0, 8], "total": 9, "week": 1794096000}])).unwrap();
        let days = &c.weeks[0];
        assert_eq!((c.total, days[0].date.as_str(), days[6].date.as_str()), (9, "2026-11-08", "2026-11-14"));
        assert_eq!((days[0].level, days[1].level, days[6].level), (0, 1, 4), "levels up to the busiest day");
        let l = languages_of(&serde_json::json!({"Shell": 10, "Rust": 990}));
        assert_eq!(l, [("Rust".to_string(), 990), ("Shell".to_string(), 10)]);
        assert_eq!(contributors_of(&serde_json::json!([{"login": "a", "contributions": 3}])), [("a".to_string(), 3)]);
        assert_eq!(lang_color("Rust"), Color::Rgb(0xde, 0xa5, 0x84));
    }

    #[test]
    fn unix_times_become_dates() {
        assert_eq!((ymd(0), ymd(1_794_539_045), ymd(951_782_400)), ((1970, 1, 1), (2026, 11, 13), (2000, 2, 29)));
        for t in [0, 86_399, 1_794_539_045, 4_102_444_800] {
            let (y, m, d) = ymd(t);
            assert_eq!(iso_time(&format!("{y}-{m:02}-{d:02}T00:00:00Z")), t - t % 86_400, "round trip {t}");
        }
    }

    #[test]
    fn gh_json_becomes_both_panes() {
        let search = serde_json::json!([{"number": 5, "title": "Elsewhere", "author": {"login": "me"}, "repository": {"name": "x", "nameWithOwner": "o/x"}, "isDraft": false, "updatedAt": "2026-11-13T03:04:05Z", "url": "u5"}]);
        let mine = prs(&search, "");
        assert_eq!((mine[0].repo.as_str(), mine[0].checks, mine[0].updated), ("o/x", Checks::None, 1_794_539_045));
        let repos = repos_of(&serde_json::json!([{"full_name": "me/r", "description": "d", "private": true, "fork": false, "archived": false, "pushed_at": "1970-01-02T00:00:00Z", "stargazers_count": 3, "forks_count": 1, "language": "Rust", "default_branch": "main", "open_issues_count": 2, "html_url": "https://github.com/me/r"}]));
        assert_eq!((repos[0].name.as_str(), repos[0].private, repos[0].stars, repos[0].pushed, repos[0].branch.as_str()), ("me/r", true, 3, 86_400, "main"));
        let repo_prs = serde_json::json!([
            {"number": 12, "title": "Fix", "author": {"login": "you"}, "isDraft": false, "headRefName": "main", "updatedAt": "", "url": "u12",
             "statusCheckRollup": [{"status": "COMPLETED", "conclusion": "SUCCESS"}, {"status": "IN_PROGRESS", "conclusion": ""}]},
            {"number": 13, "title": "Draft", "author": {"login": "me"}, "isDraft": true, "headRefName": "d", "updatedAt": "", "url": "u13",
             "statusCheckRollup": [{"state": "FAILURE"}]},
        ]);
        let p = prs(&repo_prs, "me/r");
        assert_eq!((p[0].repo.as_str(), p[0].checks, p[1].checks), ("me/r", Checks::Pending, Checks::Fail));
        let r = runs(&serde_json::json!([{"databaseId": 7, "displayTitle": "ci", "workflowName": "CI", "headBranch": "main", "status": "completed", "conclusion": "failure", "updatedAt": "1970-01-02T00:00:00Z", "url": "r7"}]));
        assert_eq!((r[0].state, r[0].updated), (RunState::Fail, 86_400));
        let n = notes(&serde_json::json!([{"reason": "mention", "updated_at": "", "subject": {"title": "T", "type": "PullRequest", "url": "https://api.github.com/repos/o/x/pulls/9"}, "repository": {"full_name": "o/x", "html_url": "https://github.com/o/x"}}]));
        assert_eq!(n[0].url, "https://github.com/o/x/pull/9");
        assert_eq!(page_url("https://ghe.example/api/v3/repos/o/x/issues/3").as_deref(), Some("https://ghe.example/o/x/issues/3"));
        // The top pane always; the bottom one once the chosen repository's lists came.
        let mut gh = GitHub::new(Some("me/r".into()), None, None);
        assert_eq!(gh.chosen.as_deref(), Some("me/r"), "the folder's repository, until one is chosen");
        gh.data = Some(Data { repos, mine, ..Data::default() });
        assert_eq!(rows(&gh, TOP)[..2], [Row::Head(Sec::Repos), Row::Repo(0)]);
        assert!(rows(&gh, BOTTOM).is_empty());
        gh.repo = Some(Ok(RepoData { prs: p, issues: Vec::new(), runs: r, ..RepoData::default() }));
        assert_eq!(rows(&gh, BOTTOM)[..2], [Row::Head(Sec::Runs), Row::Run(0)]);
        assert!(rows(&gh, BOTTOM).contains(&Row::Empty(Sec::RepoIssues)));
        assert_eq!(keys_of(&gh, Row::Pr(Sec::Mine, 0)), [Key::Edit, Key::Open], "another repository's: no checkout");
        assert_eq!(keys_of(&gh, Row::Pr(Sec::RepoPrs, 0)), [Key::Edit, Key::Checkout, Key::Open]);
        gh.here = None;
        assert_eq!(keys_of(&gh, Row::Pr(Sec::RepoPrs, 0)), [Key::Edit, Key::Open], "not the folder's: no checkout");
        gh.folded[Sec::Repos as usize] = true;
        assert!(!rows(&gh, TOP).contains(&Row::Repo(0)), "a folded section hides its rows");
    }
}
