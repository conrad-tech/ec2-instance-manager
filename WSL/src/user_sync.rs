//! Compare the user accounts on a pair of bastions and work out what it
//! would take to make them agree.
//!
//! The two boxes share `/efs/home` over EFS, and **NFS authorises by number,
//! not by name**. So an account is only usable on both if its uid *and* gid
//! are identical on both — a user whose numbers differ cannot read their own
//! home on the box that disagrees, while whoever does hold that number there
//! can.
//!
//! **The home directory is the authority**, not either passwd file. The files
//! on the shared mount already carry the numbers that matter, they are the
//! thing being protected, and both boxes see the same ones. That choice is
//! what makes most repairs cheap: aligning an *account* to its own home is a
//! `usermod`, and nothing is chowned, because the files already hold the
//! number being moved to. Aligning the home to the account would be the
//! opposite — a recursive `chown` of live data.
//!
//! What this will and will not do:
//!
//! - **Creating a missing account is safe** when the target has both numbers
//!   free. The home already exists, owned by those numbers, so the account
//!   lands on top of files that are already theirs.
//! - **Realigning an account to its home is safe** for the same reason, and
//!   is the ordinary repair for a box whose passwd drifted.
//! - **Two homes owned by one number is not repairable here.** One of them
//!   has to be renumbered, and that *is* a recursive chown of live data on a
//!   shared filesystem — the destructive case. It is reported and left.
//! - **A number held by someone else is a conflict, never an overwrite.**
//!   Taking it would need `--non-unique`, which makes two people the same
//!   identity on shared storage.

use std::collections::{BTreeMap, BTreeSet};

/// One account with a home on the shared mount, as dumped from `/etc/passwd`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
}

/// A directory under the shared `/efs/home`, by owner.
///
/// This is the authority: whatever these numbers are, that is what the
/// account must be on both boxes for the user to reach their own files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeDir {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
}

/// One bastion's answer to the dump script: the managed accounts, every uid
/// and gid in use on the box, and what the shared mount says.
///
/// The in-use maps cover far more than the managed accounts — a number can be
/// held by a local or system account with no `/efs/home` at all, and that
/// still blocks a `useradd -u`. Reporting "held by" needs the name, so they
/// are maps rather than sets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BastionUsers {
    pub accounts: Vec<Account>,
    pub uids_in_use: BTreeMap<u32, String>,
    pub gids_in_use: BTreeMap<u32, String>,
    /// Every directory under `/efs/home`, by owner. Both boxes mount the same
    /// filesystem, so these should agree; the plan reads whichever side
    /// answered.
    pub homes: Vec<HomeDir>,
}

impl BastionUsers {
    fn account(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
    }


    /// True when `id` is spent here by anything other than `name` itself.
    fn taken_by_other(&self, id: u32, name: &str) -> Option<String> {
        if let Some(who) = self.uids_in_use.get(&id) {
            if who != name {
                return Some(format!("uid {id} held by '{who}'"));
            }
        }
        if let Some(who) = self.gids_in_use.get(&id) {
            if who != name {
                return Some(format!("gid {id} held by '{who}'"));
            }
        }
        None
    }
}

/// Which number two homes are fighting over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdKind {
    Uid,
    Gid,
}

impl IdKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Uid => "uid",
            Self::Gid => "gid",
        }
    }
}

/// Which bastion a row refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Primary,
    Secondary,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
        }
    }
}

/// What should happen to one username.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Both boxes agree, and agree with the home. Nothing to do.
    Match { uid: u32, gid: u32 },
    /// Missing from `side`, and both numbers are free there.
    Add {
        side: Side,
        uid: u32,
        gid: u32,
        home: String,
    },
    /// Present on `side` but holding numbers its own home does not.
    ///
    /// Repaired with `usermod`/`groupmod` onto the home's numbers, which
    /// chowns nothing: the files already carry the destination.
    Realign {
        side: Side,
        from_uid: u32,
        from_gid: u32,
        to_uid: u32,
        to_gid: u32,
    },
    /// The two boxes disagree and there is no home to arbitrate — the
    /// directory is gone, so neither number can be called correct.
    Mismatch {
        primary_uid: u32,
        primary_gid: u32,
        secondary_uid: u32,
        secondary_gid: u32,
    },
    /// The repair needs a number that is already spent on `side`.
    Conflict {
        side: Side,
        uid: u32,
        gid: u32,
        held_by: String,
    },
    /// This home shares a number with another home on the shared mount.
    ///
    /// Not repairable here: one of the two has to be renumbered, and that is a
    /// recursive chown/chgrp of live data — which root cannot even do, since
    /// it is squashed to `nobody` on EFS. Until it is settled, whoever holds
    /// the number has the other's access.
    ///
    /// Every user is meant to have a private group: one gid per home, named
    /// after the user. A gid shared by two homes breaks that, and means each
    /// of them carries group access to the other's files.
    HomeCollision {
        what: IdKind,
        id: u32,
        others: Vec<String>,
    },
    /// The home's gid belongs to a group that is **not** named after the user
    /// — a shared group like `users`, rather than the user's own.
    ///
    /// Not repairable here either, and for a specific reason: the fix is to
    /// give the home a private gid, which means `chgrp -R` on the home. Root
    /// is squashed to `nobody` on EFS and cannot do it; only the user can.
    SharedGroup {
        side: Side,
        gid: u32,
        group: String,
    },
}

impl Action {
    /// True for the rows the apply step creates.
    pub fn is_add(&self) -> bool {
        matches!(self, Self::Add { .. })
    }

    /// True for the rows the apply step realigns.
    pub fn is_realign(&self) -> bool {
        matches!(self, Self::Realign { .. })
    }

    /// True for anything the apply step acts on.
    pub fn is_repairable(&self) -> bool {
        self.is_add() || self.is_realign()
    }

    /// True for rows that need a human.
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            Self::Mismatch { .. }
                | Self::Conflict { .. }
                | Self::HomeCollision { .. }
                | Self::SharedGroup { .. }
        )
    }

    /// Short tag for the report's first column.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Match { .. } => "OK",
            Self::Add { .. } => "ADD",
            Self::Realign { .. } => "REALIGN",
            Self::Mismatch { .. } => "DIFFERS",
            Self::Conflict { .. } => "CONFLICT",
            Self::HomeCollision { .. } => "SHARED-ID",
            Self::SharedGroup { .. } => "SHARED-GRP",
        }
    }
}

/// One username's verdict. A username can need work on both boxes, so the
/// plan carries one row per (name, action) rather than one per name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub action: Action,
}

/// Parse the dump script's output.
///
/// Unknown lines are ignored rather than failing: the output arrives over an
/// SSM session that may carry a login banner or an MOTD ahead of it, and a
/// stray line must not cost the whole comparison.
pub fn parse_dump(text: &str) -> BastionUsers {
    let mut out = BastionUsers::default();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        match f.next() {
            Some("ACCT") => {
                let (Some(name), Some(uid), Some(gid), Some(home)) =
                    (f.next(), f.next(), f.next(), f.next())
                else {
                    continue;
                };
                let (Ok(uid), Ok(gid)) = (uid.parse(), gid.parse()) else {
                    continue;
                };
                out.accounts.push(Account {
                    name: name.to_string(),
                    uid,
                    gid,
                    home: home.to_string(),
                });
            }
            Some("UIDU") => {
                if let (Some(Ok(id)), Some(name)) = (f.next().map(str::parse), f.next()) {
                    out.uids_in_use.insert(id, name.to_string());
                }
            }
            Some("GIDU") => {
                if let (Some(Ok(id)), Some(name)) = (f.next().map(str::parse), f.next()) {
                    out.gids_in_use.insert(id, name.to_string());
                }
            }
            Some("HOMEOWN") => {
                let (Some(name), Some(Ok(uid)), Some(Ok(gid))) =
                    (f.next(), f.next().map(str::parse), f.next().map(str::parse))
                else {
                    continue;
                };
                out.homes.push(HomeDir {
                    name: name.to_string(),
                    uid,
                    gid,
                });
            }
            _ => {}
        }
    }
    out.accounts.sort_by(|a, b| a.name.cmp(&b.name));
    out.homes.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The shared mount's view, taken from whichever box answered.
///
/// Both bastions mount the same filesystem, so either is the same answer; the
/// primary is preferred only because it is the box the create runs on.
fn homes_of<'a>(primary: &'a BastionUsers, secondary: &'a BastionUsers) -> &'a [HomeDir] {
    if primary.homes.is_empty() {
        &secondary.homes
    } else {
        &primary.homes
    }
}

/// Homes that share a uid — or a gid — with another home.
///
/// The gid case matters as much as the uid one: every user is meant to have a
/// private group, one gid per home named after them. Two homes on one gid
/// means each carries group access to the other's files.
///
/// Nothing about such a name can be repaired until a human settles which one
/// keeps the number.
fn collisions(homes: &[HomeDir]) -> BTreeMap<String, (IdKind, u32, Vec<String>)> {
    let mut out: BTreeMap<String, (IdKind, u32, Vec<String>)> = BTreeMap::new();
    for (kind, key) in [
        (IdKind::Uid, (|h: &HomeDir| h.uid) as fn(&HomeDir) -> u32),
        (IdKind::Gid, (|h: &HomeDir| h.gid) as fn(&HomeDir) -> u32),
    ] {
        let mut by_id: BTreeMap<u32, Vec<&HomeDir>> = BTreeMap::new();
        for h in homes {
            by_id.entry(key(h)).or_default().push(h);
        }
        for (id, group) in by_id {
            if group.len() < 2 {
                continue;
            }
            for h in &group {
                let others: Vec<String> = group
                    .iter()
                    .filter(|o| o.name != h.name)
                    .map(|o| o.name.clone())
                    .collect();
                // A uid clash is reported ahead of a gid one: it is the
                // stronger fault, and reporting both for one name would say
                // the same thing twice.
                out.entry(h.name.clone()).or_insert((kind, id, others));
            }
        }
    }
    out
}

/// Compare the two bastions and decide what each username needs.
///
/// Rows come back sorted by name so the report is stable between runs — an
/// audit that reorders itself is one nobody can diff.
pub fn plan(primary: &BastionUsers, secondary: &BastionUsers) -> Vec<Row> {
    let homes = homes_of(primary, secondary);
    let home_of = |name: &str| homes.iter().find(|h| h.name == name);
    let clashing = collisions(homes);

    let names: BTreeSet<&str> = primary
        .accounts
        .iter()
        .chain(secondary.accounts.iter())
        .map(|a| a.name.as_str())
        .chain(homes.iter().map(|h| h.name.as_str()))
        .collect();

    let mut rows = Vec::new();
    for name in names {
        // A number claimed by two homes makes every other verdict about this
        // name meaningless — "align to the home" has two answers.
        if let Some((what, id, others)) = clashing.get(name) {
            rows.push(Row {
                name: name.to_string(),
                action: Action::HomeCollision {
                    what: *what,
                    id: *id,
                    others: others.clone(),
                },
            });
            continue;
        }

        let sides = [
            (Side::Primary, primary.account(name)),
            (Side::Secondary, secondary.account(name)),
        ];

        let Some(home) = home_of(name) else {
            // No directory on the shared mount, so nothing arbitrates. Fall
            // back to comparing the boxes with each other.
            match (sides[0].1, sides[1].1) {
                (Some(p), Some(s)) if p.uid == s.uid && p.gid == s.gid => rows.push(Row {
                    name: name.to_string(),
                    action: Action::Match {
                        uid: p.uid,
                        gid: p.gid,
                    },
                }),
                (Some(p), Some(s)) => rows.push(Row {
                    name: name.to_string(),
                    action: Action::Mismatch {
                        primary_uid: p.uid,
                        primary_gid: p.gid,
                        secondary_uid: s.uid,
                        secondary_gid: s.gid,
                    },
                }),
                // Present on one box with no home at all: a local account,
                // not something this manages.
                _ => {}
            }
            continue;
        };

        let mut work = Vec::new();
        for (side, account) in sides {
            let target = match side {
                Side::Primary => primary,
                Side::Secondary => secondary,
            };
            // Every user gets a private group: the home's gid, named after the
            // user. If that gid belongs to a group with another name, it
            // matters a great deal *which* other name.
            //
            // A group belonging to another managed user whose own home says a
            // different gid is simply misplaced — it will be realigned off
            // this number, so the right answer is the ordinary Conflict below,
            // which the second pass then finds vacated. Calling that a shared
            // group would send someone to fix a thing that fixes itself.
            //
            // A group that is *not* going anywhere — a system or shared group
            // like `users` — is the real violation, and no account change
            // repairs it: the home needs its own gid, which is a chgrp only
            // the user can perform, since root is squashed to nobody on EFS.
            if let Some(group) = target.gids_in_use.get(&home.gid) {
                let will_vacate = home_of(group).is_some_and(|h| h.gid != home.gid);
                if group != name && !will_vacate {
                    work.push(Action::SharedGroup {
                        side,
                        gid: home.gid,
                        group: group.clone(),
                    });
                    continue;
                }
            }
            match account {
                Some(a) if a.uid == home.uid && a.gid == home.gid => {}
                Some(a) => {
                    // Realigning needs the home's number free here, unless it
                    // is this very account already holding part of it.
                    match target.taken_by_other(home.uid, name).or_else(|| {
                        target
                            .taken_by_other(home.gid, name)
                            .filter(|_| home.gid != home.uid)
                    }) {
                        None => work.push(Action::Realign {
                            side,
                            from_uid: a.uid,
                            from_gid: a.gid,
                            to_uid: home.uid,
                            to_gid: home.gid,
                        }),
                        Some(held_by) => work.push(Action::Conflict {
                            side,
                            uid: home.uid,
                            gid: home.gid,
                            held_by,
                        }),
                    }
                }
                None => match target.taken_by_other(home.uid, name).or_else(|| {
                    target
                        .taken_by_other(home.gid, name)
                        .filter(|_| home.gid != home.uid)
                }) {
                    None => work.push(Action::Add {
                        side,
                        uid: home.uid,
                        gid: home.gid,
                        home: format!("/efs/home/{name}"),
                    }),
                    Some(held_by) => work.push(Action::Conflict {
                        side,
                        uid: home.uid,
                        gid: home.gid,
                        held_by,
                    }),
                },
            }
        }

        if work.is_empty() {
            rows.push(Row {
                name: name.to_string(),
                action: Action::Match {
                    uid: home.uid,
                    gid: home.gid,
                },
            });
        } else {
            for action in work {
                rows.push(Row {
                    name: name.to_string(),
                    action,
                });
            }
        }
    }
    rows
}

/// The shell to create one missing account on the box it is missing from.
///
/// `-M` because the home already exists on the shared mount and is already
/// owned by these numbers; creating it is the one step that could damage data.
pub fn add_command(name: &str, uid: u32, gid: u32, home: &str) -> String {
    format!(
        "getent group {name} >/dev/null 2>&1 || groupadd -g {gid} {name}; \
         useradd -u {uid} -g {gid} -M -d {home} {name}"
    )
}

/// The shell to move an existing account onto its home's numbers.
///
/// **Nothing is chowned by design.** The files under the home already carry
/// the destination numbers — that is what makes this the safe direction. What
/// `usermod -u` does chown is anything under the home still owned by the *old*
/// uid, which is exactly the residue that should move.
///
/// `groupmod` first: `usermod` is what finally makes the account match, so
/// leaving it last means a failure part-way is still an account whose gid has
/// moved toward the target rather than away from it.
///
/// `usermod` refuses while the user has a running process, which is the right
/// answer — renumbering an account out from under a live session is how a
/// shell ends up writing files nobody owns.
pub fn realign_command(name: &str, uid: u32, gid: u32) -> String {
    format!(
        "if getent group {name} >/dev/null 2>&1; then groupmod -g {gid} {name}; \
         else groupadd -g {gid} {name}; fi; usermod -u {uid} -g {gid} {name}"
    )
}

/// Render the plan as the fixed-width report shown in the dialog and the log.
pub fn render(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "No accounts with a home under /efs/home on either bastion.".to_string();
    }
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(8).max(8);
    rows.iter()
        .map(|r| {
            let detail = match &r.action {
                Action::Match { uid, gid } if uid != gid => {
                    format!("{uid}/{gid} on both — uid≠gid")
                }
                Action::Match { uid, gid } => format!("{uid}/{gid} on both"),
                Action::Add {
                    side, uid, gid, ..
                } => format!(
                    "{uid}/{gid} — missing from the {}, both free there",
                    side.label()
                ),
                Action::Realign {
                    side,
                    from_uid,
                    from_gid,
                    to_uid,
                    to_gid,
                } => format!(
                    "{} has {from_uid}/{from_gid}, its home says {to_uid}/{to_gid} — \
                     will realign (no files move)",
                    side.label()
                ),
                Action::Mismatch {
                    primary_uid,
                    primary_gid,
                    secondary_uid,
                    secondary_gid,
                } => format!(
                    "primary {primary_uid}/{primary_gid}, secondary \
                     {secondary_uid}/{secondary_gid}, no home to arbitrate"
                ),
                Action::Conflict {
                    side,
                    uid,
                    gid,
                    held_by,
                } => format!(
                    "needs {uid}/{gid} on the {}, but {held_by}",
                    side.label()
                ),
                Action::HomeCollision { what, id, others } => format!(
                    "home shares {} {id} with {} — needs a renumber by hand",
                    what.label(),
                    others.join(", ")
                ),
                Action::SharedGroup { side, gid, group } => format!(
                    "home's gid {gid} is the shared group '{group}', not a private \
                     one named '{}' — needs a chgrp as the user on the {}",
                    r.name,
                    side.label()
                ),
            };
            format!(
                "{:<9} {:<width$}  {detail}",
                r.action.tag(),
                r.name,
                width = width
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One-line summary for the status bar and the log.
pub fn summary(rows: &[Row]) -> String {
    let adds = rows.iter().filter(|r| r.action.is_add()).count();
    let realigns = rows.iter().filter(|r| r.action.is_realign()).count();
    let attention = rows.iter().filter(|r| r.action.needs_attention()).count();
    let matched = rows.len() - adds - realigns - attention;
    format!(
        "{matched} in sync, {adds} to create, {realigns} to realign, \
         {attention} needing attention"
    )
}

/// Lowest id that is free as **both** a uid and a gid on **both** bastions,
/// and above every number either box has already spent on a shared home.
///
/// This is the number Bastion New User creates with, and it is chosen here
/// rather than by `useradd` because `useradd` can only see the box it runs on.
/// The primary picking its own lowest free uid is precisely how an account
/// came out holding a number that belonged to someone else on the secondary.
///
/// The floor comes from the managed accounts *and* the homes on both sides, so
/// a home left behind with no account anywhere — an orphan the passwd tables
/// cannot see — still cannot be handed to somebody new.
///
/// Ids at or above 60000 are ignored when taking the floor: root is squashed
/// to `nobody` (65534) on EFS, and one file owned by it would otherwise push
/// every future account past 65535.
pub fn choose_shared_id(primary: &BastionUsers, secondary: &BastionUsers) -> u32 {
    const FIRST: u32 = 1000;
    const CEILING: u32 = 60000;

    let spent = |u: &BastionUsers| -> u32 {
        u.accounts
            .iter()
            .flat_map(|a| [a.uid, a.gid])
            .chain(u.homes.iter().flat_map(|h| [h.uid, h.gid]))
            .filter(|id| *id < CEILING)
            .max()
            .unwrap_or(0)
    };
    let mut id = spent(primary).max(spent(secondary)).max(FIRST - 1) + 1;
    while primary.uids_in_use.contains_key(&id)
        || primary.gids_in_use.contains_key(&id)
        || secondary.uids_in_use.contains_key(&id)
        || secondary.gids_in_use.contains_key(&id)
    {
        id += 1;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(name: &str, uid: u32, gid: u32) -> Account {
        Account {
            name: name.to_string(),
            uid,
            gid,
            home: format!("/efs/home/{name}"),
        }
    }

    fn home(name: &str, uid: u32, gid: u32) -> HomeDir {
        HomeDir {
            name: name.to_string(),
            uid,
            gid,
        }
    }

    fn users(
        accounts: &[Account],
        extra_uids: &[(u32, &str)],
        extra_gids: &[(u32, &str)],
    ) -> BastionUsers {
        let mut u = BastionUsers {
            accounts: accounts.to_vec(),
            ..Default::default()
        };
        for a in accounts {
            u.uids_in_use.insert(a.uid, a.name.clone());
            u.gids_in_use.insert(a.gid, a.name.clone());
        }
        for (id, name) in extra_uids {
            u.uids_in_use.insert(*id, name.to_string());
        }
        for (id, name) in extra_gids {
            u.gids_in_use.insert(*id, name.to_string());
        }
        u
    }

    fn only(rows: &[Row], name: &str) -> Action {
        let mut it = rows.iter().filter(|r| r.name == name);
        let first = it.next().unwrap_or_else(|| panic!("no row for {name}")).clone();
        assert!(it.next().is_none(), "expected one row for {name}");
        first.action
    }

    #[test]
    fn accounts_that_agree_with_their_home_need_nothing() {
        let mut p = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        p.homes = vec![home("jsmith", 1004, 1004)];
        let s = p.clone();
        assert_eq!(
            only(&plan(&p, &s), "jsmith"),
            Action::Match {
                uid: 1004,
                gid: 1004
            }
        );
    }

    /// The drift that started all this: an account made on one box only.
    #[test]
    fn an_account_missing_from_one_box_is_added_with_the_homes_numbers() {
        let mut p = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        p.homes = vec![home("jsmith", 1004, 1004), home("mbravo", 1011, 1011)];
        let mut s = users(
            &[acct("jsmith", 1004, 1004), acct("mbravo", 1011, 1011)],
            &[],
            &[],
        );
        s.homes = p.homes.clone();
        assert_eq!(
            only(&plan(&p, &s), "mbravo"),
            Action::Add {
                side: Side::Primary,
                uid: 1011,
                gid: 1011,
                home: "/efs/home/mbravo".to_string(),
            }
        );
    }

    /// The repair this question was about: the account disagrees with its own
    /// home, so it is moved onto the home's numbers. No files move — they
    /// already carry the destination.
    #[test]
    fn an_account_that_disagrees_with_its_home_is_realigned() {
        let mut p = users(&[acct("bwilson", 1007, 1007)], &[], &[]);
        p.homes = vec![home("bwilson", 1009, 1009)];
        let mut s = users(&[acct("bwilson", 1009, 1009)], &[], &[]);
        s.homes = p.homes.clone();
        let rows = plan(&p, &s);
        assert_eq!(
            only(&rows, "bwilson"),
            Action::Realign {
                side: Side::Primary,
                from_uid: 1007,
                from_gid: 1007,
                to_uid: 1009,
                to_gid: 1009,
            }
        );
        assert!(rows[0].action.is_repairable());
        assert!(!rows[0].action.needs_attention());
    }

    /// A realign that would need a number somebody else holds is refused —
    /// taking it would need --non-unique.
    #[test]
    fn a_realign_onto_a_taken_number_becomes_a_conflict() {
        let mut p = users(&[acct("bwilson", 1007, 1007)], &[(1009, "someone")], &[]);
        p.homes = vec![home("bwilson", 1009, 1009)];
        let mut s = users(&[acct("bwilson", 1009, 1009)], &[], &[]);
        s.homes = p.homes.clone();
        match only(&plan(&p, &s), "bwilson") {
            Action::Conflict { side, held_by, .. } => {
                assert_eq!(side, Side::Primary);
                assert!(held_by.contains("someone"), "{held_by}");
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    /// Exactly the state the bastions are in now: two homes owned by one
    /// number. Unrepairable here — one of them must be renumbered, which is
    /// the recursive chown of live data this deliberately will not do.
    #[test]
    fn two_homes_sharing_a_uid_are_reported_not_repaired() {
        let mut p = users(&[acct("swilson", 1011, 1012)], &[], &[]);
        p.homes = vec![home("swilson", 1011, 1012), home("mbravo", 1011, 1011)];
        let s = p.clone();
        let rows = plan(&p, &s);
        for name in ["swilson", "mbravo"] {
            match only(&rows, name) {
                Action::HomeCollision { what, id, others } => {
                    assert_eq!(what, IdKind::Uid);
                    assert_eq!(id, 1011);
                    assert!(!others.is_empty(), "should name the other home");
                }
                other => panic!("{name}: expected a collision, got {other:?}"),
            }
        }
        // Nothing about a collided name may be auto-repaired.
        assert!(rows.iter().all(|r| !r.action.is_repairable()));
        assert!(rows.iter().all(|r| r.action.needs_attention()));
    }

    /// "Primary has 1010 for john, secondary has 1010 for mike" — two
    /// *accounts* on different boxes holding one number. That is NOT the
    /// shared-id case: their homes carry different numbers, so the mount still
    /// says who each of them should be. Mike is simply wrong on the secondary.
    ///
    /// It does need two passes, though: john cannot take 1010 on the secondary
    /// until mike has vacated it, and the plan is computed from one snapshot.
    #[test]
    fn one_number_held_by_two_accounts_on_different_boxes() {
        let homes = vec![home("john", 1010, 1010), home("mike", 1015, 1015)];
        let mut p = users(&[acct("john", 1010, 1010)], &[], &[]);
        p.homes = homes.clone();
        let mut s = users(&[acct("mike", 1010, 1010)], &[], &[]);
        s.homes = homes.clone();

        let rows = plan(&p, &s);
        // Nobody is a shared-id: the two homes carry different numbers.
        assert!(
            !rows.iter().any(|r| matches!(r.action, Action::HomeCollision { .. })),
            "{rows:?}"
        );
        // Mike holds 1010 on the secondary but his home says 1015.
        assert!(rows.iter().any(|r| r.name == "mike"
            && matches!(
                r.action,
                Action::Realign {
                    side: Side::Secondary,
                    to_uid: 1015,
                    ..
                }
            )));
        // John is missing from the secondary and blocked *by mike*, until
        // mike moves off 1010.
        assert!(rows.iter().any(|r| r.name == "john"
            && matches!(&r.action, Action::Conflict { side: Side::Secondary, held_by, .. }
                if held_by.contains("mike"))));

        // Second pass, with mike realigned: john's add is now clear.
        let mut s2 = users(&[acct("mike", 1015, 1015)], &[], &[]);
        s2.homes = homes;
        assert!(plan(&p, &s2).iter().any(|r| r.name == "john"
            && matches!(r.action, Action::Add { side: Side::Secondary, uid: 1010, .. })));
    }

    /// The shared-id case is about the MOUNT, not the passwd files: two
    /// directories under /efs/home owned by one number. That is what has no
    /// answer, because "align the account to its home" gives two.
    #[test]
    fn the_shared_id_case_is_two_homes_not_two_accounts() {
        let mut p = users(&[acct("john", 1010, 1010)], &[], &[]);
        p.homes = vec![home("john", 1010, 1010), home("mike", 1010, 1010)];
        let rows = plan(&p, &p.clone());
        assert!(rows
            .iter()
            .all(|r| matches!(r.action, Action::HomeCollision { .. })));
    }

    /// Every user is meant to have a private group: one gid per home, named
    /// after the user. Two homes on one gid breaks that — each carries group
    /// access to the other's files — and it cannot be repaired from here,
    /// because the fix is a chgrp of live data.
    #[test]
    fn two_homes_sharing_a_gid_are_reported() {
        let mut p = users(&[], &[], &[]);
        p.homes = vec![home("john", 1010, 1099), home("mike", 1011, 1099)];
        let rows = plan(&p, &p.clone());
        for name in ["john", "mike"] {
            match only(&rows, name) {
                Action::HomeCollision { what, id, others } => {
                    assert_eq!(what, IdKind::Gid);
                    assert_eq!(id, 1099);
                    assert!(!others.is_empty());
                }
                other => panic!("{name}: expected a gid collision, got {other:?}"),
            }
        }
        assert!(rows.iter().all(|r| !r.action.is_repairable()));
    }

    /// A home whose gid is a shared group (`users`) rather than the user's own
    /// private one. No account change fixes it — the home needs its own gid,
    /// and that chgrp can only be done by the user, since root is squashed to
    /// nobody on EFS.
    #[test]
    fn a_home_group_that_is_not_the_users_own_is_reported() {
        let mut p = users(&[acct("john", 1010, 100)], &[], &[(100, "users")]);
        p.homes = vec![home("john", 1010, 100)];
        let rows = plan(&p, &p.clone());
        // One per box: the violation is on both, and each needs its own fix.
        assert_eq!(rows.len(), 2, "{rows:?}");
        for row in &rows {
            match &row.action {
                Action::SharedGroup { gid, group, .. } => {
                    assert_eq!(*gid, 100);
                    assert_eq!(group, "users");
                }
                other => panic!("expected a shared group, got {other:?}"),
            }
            assert!(row.action.needs_attention());
            assert!(!row.action.is_repairable(), "a chgrp is not done from here");
        }
    }

    /// ...but a group merely sitting on the number *because it is misplaced*
    /// is not that. It gets realigned off, and the second pass finds the
    /// number free — so calling it a shared group would send someone to fix
    /// something that fixes itself.
    #[test]
    fn a_group_that_will_be_realigned_away_is_not_a_shared_group() {
        let homes = vec![home("john", 1010, 1010), home("mike", 1015, 1015)];
        let mut p = users(&[acct("john", 1010, 1010)], &[], &[]);
        p.homes = homes.clone();
        let mut s = users(&[acct("mike", 1010, 1010)], &[], &[]);
        s.homes = homes;
        let rows = plan(&p, &s);
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r.action, Action::SharedGroup { .. })),
            "mike's group is misplaced, not shared: {rows:?}"
        );
    }

    /// The realign has to work on a box that has no group for the user at all
    /// — half a mirror leaves exactly that — or `usermod -g` fails against a
    /// gid no group holds.
    #[test]
    fn realign_creates_the_private_group_when_the_box_has_none() {
        let cmd = realign_command("bwilson", 1009, 1009);
        assert!(cmd.contains("groupadd -g 1009 bwilson"), "{cmd}");
        assert!(cmd.contains("groupmod -g 1009 bwilson"), "{cmd}");
        assert!(cmd.contains("usermod -u 1009 -g 1009 bwilson"), "{cmd}");
    }

    /// With the directory gone there is no authority, so neither number can
    /// be called correct and it stays a report.
    #[test]
    fn without_a_home_a_disagreement_is_only_reported() {
        let p = users(&[acct("ghost", 1007, 1007)], &[], &[]);
        let s = users(&[acct("ghost", 1009, 1009)], &[], &[]);
        let action = only(&plan(&p, &s), "ghost");
        assert!(matches!(action, Action::Mismatch { .. }));
        assert!(action.needs_attention());
        assert!(!action.is_repairable());
    }

    /// A name can need work on both boxes at once.
    #[test]
    fn a_name_can_need_a_repair_on_each_box() {
        let mut p = users(&[acct("split", 1007, 1007)], &[], &[]);
        p.homes = vec![home("split", 1020, 1020)];
        let mut s = users(&[], &[], &[]);
        s.homes = p.homes.clone();
        let rows = plan(&p, &s);
        let actions: Vec<&Action> = rows.iter().map(|r| &r.action).collect();
        assert_eq!(actions.len(), 2, "{actions:?}");
        assert!(actions.iter().any(|a| a.is_realign()));
        assert!(actions.iter().any(|a| a.is_add()));
    }

    /// An orphaned home — no account on either box — is still reported, since
    /// its number is spoken for and its files belong to somebody.
    #[test]
    fn an_orphaned_home_is_surfaced_as_work() {
        let mut p = users(&[], &[], &[]);
        p.homes = vec![home("departed", 1042, 1042)];
        let s = p.clone();
        let rows = plan(&p, &s);
        assert_eq!(rows.len(), 2, "one per box: {rows:?}");
        assert!(rows.iter().all(|r| r.action.is_add()));
    }

    #[test]
    fn rows_are_sorted_by_name() {
        let mut p = users(&[acct("zeta", 1002, 1002), acct("alpha", 1003, 1003)], &[], &[]);
        p.homes = vec![home("alpha", 1003, 1003), home("zeta", 1002, 1002)];
        let rows = plan(&p, &p.clone());
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[test]
    fn parse_dump_reads_all_four_record_types() {
        let text = "\
Last login: Tue Aug 11 20:45:01 2026 from 10.0.0.1
ACCT jsmith 1004 1004 /efs/home/jsmith
UIDU 1004 jsmith
UIDU 65534 nobody
GIDU 100 users
HOMEOWN jsmith 1004 1004
HOMEOWN mbravo 1011 1011
";
        let u = parse_dump(text);
        assert_eq!(u.accounts.len(), 1);
        assert_eq!(u.uids_in_use.get(&65534).map(String::as_str), Some("nobody"));
        assert_eq!(u.gids_in_use.get(&100).map(String::as_str), Some("users"));
        assert_eq!(u.homes, vec![home("jsmith", 1004, 1004), home("mbravo", 1011, 1011)]);
    }

    /// A banner, an MOTD or a stray prompt must not cost the comparison.
    #[test]
    fn parse_dump_ignores_noise_and_malformed_rows() {
        let text = "\
### banner ###
ACCT broken
ACCT jsmith notanumber 1004 /efs/home/jsmith
UIDU alsobroken
HOMEOWN nameonly
ACCT good 1005 1005 /efs/home/good
HOMEOWN good 1005 1005
";
        let u = parse_dump(text);
        assert_eq!(u.accounts.len(), 1);
        assert_eq!(u.accounts[0].name, "good");
        assert_eq!(u.homes, vec![home("good", 1005, 1005)]);
    }

    #[test]
    fn add_command_never_creates_the_home() {
        let cmd = add_command("mbravo", 1011, 1011, "/efs/home/mbravo");
        assert!(cmd.contains("useradd -u 1011 -g 1011 -M -d /efs/home/mbravo mbravo"));
        assert!(!cmd.contains("-m "), "the home already exists: {cmd}");
        assert!(!cmd.contains("-o") && !cmd.contains("non-unique"));
        assert!(cmd.contains("getent group mbravo >/dev/null 2>&1 || groupadd -g 1011 mbravo"));
    }

    /// The realign must never chown: the files already carry the destination,
    /// and a recursive chown of a live home is the thing being avoided.
    #[test]
    fn realign_command_moves_the_account_and_not_the_files() {
        let cmd = realign_command("bwilson", 1009, 1009);
        assert!(cmd.contains("usermod -u 1009 -g 1009 bwilson"));
        assert!(cmd.contains("groupmod -g 1009 bwilson"));
        assert!(!cmd.contains("chown"), "must not touch the files: {cmd}");
        assert!(!cmd.contains("-o") && !cmd.contains("non-unique"));
    }

    #[test]
    fn summary_counts_each_category() {
        let mut p = users(&[acct("ok", 1002, 1002), acct("moveme", 1003, 1003)], &[], &[]);
        p.homes = vec![
            home("ok", 1002, 1002),
            home("moveme", 1030, 1030),
            home("addme", 1031, 1031),
        ];
        let mut s = users(&[acct("ok", 1002, 1002), acct("moveme", 1030, 1030)], &[], &[]);
        s.homes = p.homes.clone();
        let rows = plan(&p, &s);
        // ok matches; moveme realigns on the primary; addme is missing from
        // both, so it adds twice.
        assert_eq!(summary(&rows), "1 in sync, 2 to create, 1 to realign, 0 needing attention");
    }

    #[test]
    fn render_names_every_row_and_stays_stable() {
        let mut p = users(&[acct("bwilson", 1007, 1007)], &[], &[]);
        p.homes = vec![home("bwilson", 1009, 1009)];
        let mut s = users(&[acct("bwilson", 1009, 1009)], &[], &[]);
        s.homes = p.homes.clone();
        let rows = plan(&p, &s);
        let out = render(&rows);
        assert!(out.contains("REALIGN") && out.contains("bwilson"));
        assert!(out.contains("no files move"));
        assert_eq!(out, render(&plan(&p, &s)), "render must be deterministic");
    }

    #[test]
    fn an_empty_pair_says_so_rather_than_rendering_nothing() {
        assert!(render(&[]).contains("No accounts"));
    }

    /// The incident, as an allocation question: the primary had uid 1011 free
    /// while the secondary had spent it on someone else.
    #[test]
    fn the_chosen_id_clears_a_number_spent_on_the_other_bastion() {
        let primary = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        let secondary = users(
            &[acct("jsmith", 1004, 1004), acct("mbravo", 1011, 1011)],
            &[],
            &[],
        );
        assert_eq!(choose_shared_id(&primary, &secondary), 1012);
    }

    #[test]
    fn the_chosen_id_is_free_as_a_uid_and_a_gid_on_both_sides() {
        let primary = users(&[], &[(1002, "a")], &[(1003, "b")]);
        let secondary = users(&[], &[(1004, "c")], &[(1005, "d")]);
        assert_eq!(choose_shared_id(&primary, &secondary), 1000);
        let primary = users(&[], &[(1000, "a"), (1002, "c")], &[(1001, "b")]);
        assert_eq!(choose_shared_id(&primary, &secondary), 1003);
    }

    /// A home whose account is gone on both boxes is invisible to the passwd
    /// tables. Handing its number out would give its files to somebody new.
    #[test]
    fn an_orphaned_home_still_reserves_its_number() {
        let mut primary = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        primary.homes = vec![home("jsmith", 1004, 1004), home("departed", 1042, 1042)];
        let secondary = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        assert!(choose_shared_id(&primary, &secondary) > 1042);
    }

    /// Root is squashed to nobody (65534) on EFS, so one file owned by it must
    /// not push every future account past 65535.
    #[test]
    fn the_squash_owner_does_not_drag_the_floor_up() {
        let mut primary = users(&[acct("jsmith", 1004, 1004)], &[(65534, "nobody")], &[]);
        primary.homes = vec![home("squashed", 65534, 65534)];
        let secondary = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        assert_eq!(choose_shared_id(&primary, &secondary), 1005);
    }

    #[test]
    fn a_pair_with_no_accounts_starts_at_the_first_user_id() {
        assert_eq!(
            choose_shared_id(&BastionUsers::default(), &BastionUsers::default()),
            1000
        );
    }
}
