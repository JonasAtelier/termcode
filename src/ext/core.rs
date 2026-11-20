//! The code views as drivers: Explorer, Search and Source Control. They plug in through
//! `Ops` as any view does (their side bar, title, icon, badge, keys); they share the code
//! editor area (`Ops::code`), so they sit at the top of the activity bar. Their lists
//! are the panel's own (panel/sidebar.rs, ui::list_side, scm/).

use super::Ops;
use crate::theme::icons;

pub(super) const EXPLORER: Ops = Ops {
    id: "explorer",
    name: "Explorer",
    title: "EXPLORER",
    about: "The workspace's files, and the timeline of the one open",
    icon: || icons().files,
    side: crate::ui::list_side,
    code: true,
    ..Ops::NONE
};

pub(super) const SEARCH: Ops = Ops {
    id: "search",
    name: "Search",
    title: "SEARCH",
    about: "Text across the workspace, and a symbol's references",
    icon: || icons().search,
    side: crate::ui::list_side,
    code: true,
    ..Ops::NONE
};

pub(super) const SCM: Ops = Ops {
    id: "scm",
    name: "Source Control",
    title: "SOURCE CONTROL",
    about: "Changes, commits and the graph, through git",
    icon: || icons().scm,
    side: crate::scm::draw_scm,
    key: crate::scm::key,
    badge: |app, _| app.scm_files.len(),
    code: true,
    ..Ops::NONE
};
