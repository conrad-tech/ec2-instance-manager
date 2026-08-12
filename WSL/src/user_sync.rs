//! Compare the user accounts on a pair of bastions and work out what it
//! would take to make them agree.
//!
//! The two boxes share `/efs/home` over EFS, and **NFS authorises by number,
//! not by name**. So an account is only usable on both if its uid *and* gid
//! are identical on both — a user whose numbers differ cannot read their own
//! home on the box that disagrees, while whoever does hold that number there
//! can.
//!
//! Drift arrives when an account is made on one box and not the other. That
//! box then hands the next new user a number the other box has already spent,
//! which is what [`crate::features`]-gated Bastion New User now avoids by
//! allocating above every `/efs/home` owner — but it cannot repair the
//! divergence that already exists. This module plans that repair.
//!
//! What it will and will not do is the whole point:
//!
//! - **Creating a missing account is safe** when the target has both numbers
//!   free. The home already exists on the shared mount, owned by those
//!   numbers, so the new account lands on top of files that are already
//!   theirs.
//! - **Renumbering is not attempted.** Correcting a uid mismatch means
//!   `usermod -u` plus a recursive `chown` of a live home on a shared
//!   filesystem, which is destructive, cannot run while the user has files
//!   open, and hands one person's home to another if the number is wrong.
//!   Those rows are reported for a human to decide on.
//! - **A number held by someone else is a conflict, never an overwrite.**
//!   Creating the account anyway would need `--non-unique`, which makes two
//!   people the same identity on a shared filesystem.

use std::collections::{BTreeMap, BTreeSet};

/// One account with a home on the shared mount, as dumped from `/etc/passwd`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
}

/// One bastion's answer to the dump script: the managed accounts, plus every
/// uid and gid in use on the box.
///
/// The in-use sets cover far more than the managed accounts — a uid can be
/// held by a local or system account with no `/efs/home` at all, and that
/// still blocks a `useradd -u`. Reporting "held by" needs the name, so these
/// are maps rather than sets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BastionUsers {
    pub accounts: Vec<Account>,
    pub uids_in_use: BTreeMap<u32, String>,
    pub gids_in_use: BTreeMap<u32, String>,
    /// `(uid, gid)` of every directory under the shared `/efs/home`.
    ///
    /// Separate from `accounts` because a home can outlive its account — a
    /// deletion on both boxes leaves the directory owned by a number no
    /// passwd entry mentions. `choose_shared_id` floors above these so that
    /// number is never handed to somebody new, who would then own the files.
    pub home_owners: Vec<(u32, u32)>,
}

impl BastionUsers {
    fn account(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
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
    /// Present on both with matching numbers. Nothing to do.
    Match { uid: u32, gid: u32 },
    /// Missing from `missing_from`, and both numbers are free there.
    Add {
        missing_from: Side,
        uid: u32,
        gid: u32,
        home: String,
    },
    /// Present on both, but the numbers disagree. Needs a renumber, which is
    /// deliberately out of scope.
    Mismatch {
        primary_uid: u32,
        primary_gid: u32,
        secondary_uid: u32,
        secondary_gid: u32,
    },
    /// Missing from `missing_from`, but a number it needs is already spent
    /// there by something else.
    Conflict {
        missing_from: Side,
        uid: u32,
        gid: u32,
        /// Human-readable account of what holds the number(s).
        held_by: String,
    },
}

impl Action {
    /// True for the rows the apply step acts on.
    pub fn is_add(&self) -> bool {
        matches!(self, Self::Add { .. })
    }

    /// True for rows that need a human — a divergence this will not repair.
    pub fn needs_attention(&self) -> bool {
        matches!(self, Self::Mismatch { .. } | Self::Conflict { .. })
    }

    /// Short tag for the report's first column.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Match { .. } => "OK",
            Self::Add { .. } => "ADD",
            Self::Mismatch { .. } => "DIFFERS",
            Self::Conflict { .. } => "CONFLICT",
        }
    }
}

/// One username's verdict.
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
                if let (Some(Ok(uid)), Some(Ok(gid))) =
                    (f.next().map(str::parse), f.next().map(str::parse))
                {
                    out.home_owners.push((uid, gid));
                }
            }
            _ => {}
        }
    }
    out.accounts.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Describe what already holds `uid`/`gid` on the box an account is missing
/// from. Returns `None` when both are free, which is what makes the add safe.
fn holder(target: &BastionUsers, uid: u32, gid: u32, name: &str) -> Option<String> {
    let mut parts = Vec::new();
    // A group of the account's own name is not a conflict when it already
    // carries the right gid — a half-finished mirror leaves exactly that, and
    // refusing it would strand the very rows this exists to repair.
    if let Some(who) = target.uids_in_use.get(&uid) {
        parts.push(format!("uid {uid} held by '{who}'"));
    }
    if let Some(who) = target.gids_in_use.get(&gid) {
        if who != name {
            parts.push(format!("gid {gid} held by '{who}'"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// Compare the two bastions and decide what each username needs.
///
/// Rows come back sorted by name so the report is stable between runs — an
/// audit that reorders itself is one nobody can diff.
pub fn plan(primary: &BastionUsers, secondary: &BastionUsers) -> Vec<Row> {
    let names: BTreeSet<&str> = primary
        .accounts
        .iter()
        .chain(secondary.accounts.iter())
        .map(|a| a.name.as_str())
        .collect();

    names
        .into_iter()
        .map(|name| {
            let action = match (primary.account(name), secondary.account(name)) {
                (Some(p), Some(s)) => {
                    if p.uid == s.uid && p.gid == s.gid {
                        Action::Match {
                            uid: p.uid,
                            gid: p.gid,
                        }
                    } else {
                        Action::Mismatch {
                            primary_uid: p.uid,
                            primary_gid: p.gid,
                            secondary_uid: s.uid,
                            secondary_gid: s.gid,
                        }
                    }
                }
                (Some(p), None) => missing(p, Side::Secondary, secondary),
                (None, Some(s)) => missing(s, Side::Primary, primary),
                // Unreachable: the name came from one of the two lists.
                (None, None) => unreachable!("name came from one of the two account lists"),
            };
            Row {
                name: name.to_string(),
                action,
            }
        })
        .collect()
}

/// Decide between Add and Conflict for an account absent from one side.
fn missing(have: &Account, missing_from: Side, target: &BastionUsers) -> Action {
    match holder(target, have.uid, have.gid, &have.name) {
        None => Action::Add {
            missing_from,
            uid: have.uid,
            gid: have.gid,
            home: have.home.clone(),
        },
        Some(held_by) => Action::Conflict {
            missing_from,
            uid: have.uid,
            gid: have.gid,
            held_by,
        },
    }
}

/// The shell to create one missing account on the box it is missing from.
///
/// `-M` because the home already exists on the shared mount and is already
/// owned by these numbers; creating it would be the one step that could
/// damage data. The numbers are passed explicitly for the same reason the
/// allocator in `create_new_user.sh` exists — letting `useradd` choose is how
/// the two boxes drifted apart in the first place.
pub fn add_command(name: &str, uid: u32, gid: u32, home: &str) -> String {
    format!(
        "getent group {name} >/dev/null 2>&1 || groupadd -g {gid} {name}; \
         useradd -u {uid} -g {gid} -M -d {home} {name}"
    )
}

/// Render the plan as the fixed-width report shown in the dialog and written
/// to the log.
pub fn render(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "No accounts with a home under /efs/home on either bastion.".to_string();
    }
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(8).max(8);
    rows.iter()
        .map(|r| {
            let detail = match &r.action {
                // Both boxes agreeing is the question this answers, so an
                // account whose own uid and gid differ is still "in sync".
                // It is worth naming anyway: two independent allocators are
                // how that happens, and it is the shape the drift starts in.
                Action::Match { uid, gid } if uid != gid => {
                    format!("{uid}/{gid} on both — uid≠gid")
                }
                Action::Match { uid, gid } => format!("{uid}/{gid} on both"),
                Action::Add {
                    missing_from,
                    uid,
                    gid,
                    ..
                } => format!(
                    "{uid}/{gid} — missing from {}, both free there",
                    missing_from.label()
                ),
                Action::Mismatch {
                    primary_uid,
                    primary_gid,
                    secondary_uid,
                    secondary_gid,
                } => format!(
                    "primary {primary_uid}/{primary_gid}, secondary \
                     {secondary_uid}/{secondary_gid} — needs a renumber by hand"
                ),
                Action::Conflict {
                    missing_from,
                    uid,
                    gid,
                    held_by,
                } => format!(
                    "{uid}/{gid} — missing from {}, but {held_by}",
                    missing_from.label()
                ),
            };
            format!(
                "{:<8} {:<width$}  {detail}",
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
    let attention = rows.iter().filter(|r| r.action.needs_attention()).count();
    let matched = rows.len() - adds - attention;
    format!("{matched} in sync, {adds} to create, {attention} needing attention")
}

/// The side an add targets, for the apply step.
pub fn adds_for(rows: &[Row], side: Side) -> Vec<&Row> {
    rows.iter()
        .filter(|r| matches!(&r.action, Action::Add { missing_from, .. } if *missing_from == side))
        .collect()
}

/// Lowest id that is free as **both** a uid and a gid on **both** bastions,
/// and above every number either box has already spent on a shared home.
///
/// This is the number Bastion New User creates with, and it is chosen here
/// rather than by `useradd` because `useradd` can only see the box it runs on.
/// The primary picking its own lowest free uid is precisely how an account
/// came out holding a number that belonged to someone else on the secondary.
///
/// The floor comes from the managed accounts *and* the home owners on both
/// sides, so a home left behind with no account anywhere — an orphan the
/// account tables cannot see — still cannot be handed to somebody new.
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
            .chain(u.home_owners.iter().flat_map(|(uid, gid)| [*uid, *gid]))
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

    #[test]
    fn matching_accounts_need_nothing() {
        let p = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        let s = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        let rows = plan(&p, &s);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].action,
            Action::Match {
                uid: 1004,
                gid: 1004
            }
        );
        assert!(!rows[0].action.needs_attention());
    }

    /// The incident this exists for: an account made on the secondary only.
    /// It is addable because the primary has not spent those numbers.
    #[test]
    fn account_on_one_side_only_is_added_to_the_other() {
        let p = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        let s = users(&[acct("jsmith", 1004, 1004), acct("mbravo", 1011, 1011)], &[], &[]);
        let rows = plan(&p, &s);
        let mbravo = rows.iter().find(|r| r.name == "mbravo").expect("row");
        assert_eq!(
            mbravo.action,
            Action::Add {
                missing_from: Side::Primary,
                uid: 1011,
                gid: 1011,
                home: "/efs/home/mbravo".to_string(),
            }
        );
        assert_eq!(adds_for(&rows, Side::Primary).len(), 1);
        assert_eq!(adds_for(&rows, Side::Secondary).len(), 0);
    }

    /// Numbers that differ are never silently "fixed": that means a usermod
    /// plus a recursive chown of a live home on a shared filesystem.
    #[test]
    fn differing_numbers_are_reported_not_repaired() {
        let p = users(&[acct("bwilson", 1007, 1007)], &[], &[]);
        let s = users(&[acct("bwilson", 1009, 1009)], &[], &[]);
        let rows = plan(&p, &s);
        assert_eq!(
            rows[0].action,
            Action::Mismatch {
                primary_uid: 1007,
                primary_gid: 1007,
                secondary_uid: 1009,
                secondary_gid: 1009,
            }
        );
        assert!(rows[0].action.needs_attention());
        assert!(!rows[0].action.is_add());
        assert_eq!(adds_for(&rows, Side::Primary).len(), 0);
        assert_eq!(adds_for(&rows, Side::Secondary).len(), 0);
    }

    /// A uid spent by a *different* account on the target blocks the add.
    /// Creating it anyway needs --non-unique, which makes two people one
    /// identity on a shared filesystem.
    #[test]
    fn a_uid_held_by_someone_else_is_a_conflict() {
        let p = users(&[acct("tjones", 1013, 1013)], &[], &[]);
        // The secondary spent 1013 on an account with no /efs/home at all,
        // so it does not appear in `accounts` — only in the in-use map.
        let s = users(&[], &[(1013, "localsvc")], &[]);
        let rows = plan(&p, &s);
        match &rows[0].action {
            Action::Conflict {
                missing_from,
                held_by,
                ..
            } => {
                assert_eq!(*missing_from, Side::Secondary);
                assert!(held_by.contains("localsvc"), "should name the holder: {held_by}");
                assert!(held_by.contains("1013"));
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert!(rows[0].action.needs_attention());
    }

    /// A half-finished mirror leaves the group behind with the right gid.
    /// That is the state this repairs, so it must not be read as a conflict.
    #[test]
    fn the_accounts_own_leftover_group_is_not_a_conflict() {
        let p = users(&[acct("swilson", 1012, 1012)], &[], &[]);
        let s = users(&[], &[], &[(1012, "swilson")]);
        let rows = plan(&p, &s);
        assert!(
            rows[0].action.is_add(),
            "a leftover group of the same name must not block the add: {:?}",
            rows[0].action
        );
    }

    /// ...but a group of that gid belonging to someone else still does.
    #[test]
    fn a_gid_held_by_a_different_group_is_a_conflict() {
        let p = users(&[acct("swilson", 1012, 1012)], &[], &[]);
        let s = users(&[], &[], &[(1012, "othergroup")]);
        let rows = plan(&p, &s);
        assert!(rows[0].action.needs_attention());
    }

    #[test]
    fn rows_are_sorted_by_name() {
        let p = users(&[acct("zeta", 1002, 1002), acct("alpha", 1003, 1003)], &[], &[]);
        let rows = plan(&p, &p.clone());
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[test]
    fn parse_dump_reads_all_three_record_types() {
        let text = "\
Last login: Tue Aug 11 20:45:01 2026 from 10.0.0.1
ACCT jsmith 1004 1004 /efs/home/jsmith
ACCT mbravo 1011 1011 /efs/home/mbravo
UIDU 1004 jsmith
UIDU 1011 mbravo
UIDU 65534 nobody
GIDU 1004 jsmith
GIDU 100 users
";
        let u = parse_dump(text);
        assert_eq!(u.accounts.len(), 2);
        assert_eq!(u.accounts[0].name, "jsmith");
        assert_eq!(u.accounts[1].uid, 1011);
        assert_eq!(u.accounts[1].home, "/efs/home/mbravo");
        assert_eq!(u.uids_in_use.get(&65534).map(String::as_str), Some("nobody"));
        assert_eq!(u.gids_in_use.get(&100).map(String::as_str), Some("users"));
    }

    /// A banner, an MOTD or a stray prompt must not cost the comparison.
    #[test]
    fn parse_dump_ignores_noise_and_malformed_rows() {
        let text = "\
### banner ###
ACCT broken
ACCT jsmith notanumber 1004 /efs/home/jsmith
UIDU alsobroken
ACCT good 1005 1005 /efs/home/good
";
        let u = parse_dump(text);
        assert_eq!(u.accounts.len(), 1);
        assert_eq!(u.accounts[0].name, "good");
    }

    #[test]
    fn add_command_never_creates_the_home() {
        let cmd = add_command("mbravo", 1011, 1011, "/efs/home/mbravo");
        assert!(cmd.contains("useradd -u 1011 -g 1011 -M -d /efs/home/mbravo mbravo"));
        assert!(
            !cmd.contains("-m "),
            "the home is on the shared mount and already exists: {cmd}"
        );
        // Never --non-unique: that is the one flag that would make two people
        // the same identity on EFS.
        assert!(!cmd.contains("-o") && !cmd.contains("non-unique"));
        // An existing group of the right name is reused rather than fought.
        assert!(cmd.contains("getent group mbravo >/dev/null 2>&1 || groupadd -g 1011 mbravo"));
    }

    #[test]
    fn summary_counts_each_category() {
        let p = users(
            &[acct("ok", 1002, 1002), acct("addme", 1003, 1003), acct("diff", 1004, 1004)],
            &[],
            &[],
        );
        let s = users(&[acct("ok", 1002, 1002), acct("diff", 1009, 1009)], &[], &[]);
        let rows = plan(&p, &s);
        assert_eq!(summary(&rows), "1 in sync, 1 to create, 1 needing attention");
    }

    #[test]
    fn render_names_every_row_and_stays_stable() {
        let p = users(&[acct("jsmith", 1004, 1004), acct("mbravo", 1011, 1011)], &[], &[]);
        let s = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        let rows = plan(&p, &s);
        let out = render(&rows);
        assert!(out.contains("OK") && out.contains("jsmith"));
        assert!(out.contains("ADD") && out.contains("mbravo"));
        assert!(out.contains("missing from secondary"));
        assert_eq!(out, render(&plan(&p, &s)), "render must be deterministic");
    }

    /// The account this whole investigation started from came out 1011/1012,
    /// because groupadd and useradd each picked their own lowest free number.
    /// Both boxes agree on it, so it is in sync — and still worth naming.
    #[test]
    fn an_account_whose_uid_and_gid_differ_is_called_out() {
        let p = users(&[acct("swilson", 1011, 1012)], &[], &[]);
        let rows = plan(&p, &p.clone());
        assert!(matches!(rows[0].action, Action::Match { .. }));
        assert!(
            render(&rows).contains("uid≠gid"),
            "should name the split: {}",
            render(&rows)
        );
        // An even account must not pick up the note.
        let even = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        assert!(!render(&plan(&even, &even.clone())).contains("uid≠gid"));
    }

    #[test]
    fn an_empty_pair_says_so_rather_than_rendering_nothing() {
        assert!(render(&[]).contains("No accounts"));
    }

    /// The incident, as an allocation question: the primary had uid 1011 free
    /// while the secondary had spent it on someone else. Choosing from the
    /// primary alone is what produced an account the secondary refused.
    #[test]
    fn the_chosen_id_clears_a_number_spent_on_the_other_bastion() {
        let primary = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        let secondary = users(
            &[acct("jsmith", 1004, 1004), acct("mbravo", 1011, 1011)],
            &[],
            &[],
        );
        let id = choose_shared_id(&primary, &secondary);
        assert!(id > 1011, "must clear the secondary's 1011, got {id}");
        assert_eq!(id, 1012);
    }

    /// The other half of the same bug: a gid spent here and a uid spent there
    /// must both be cleared, since one number has to serve as both.
    #[test]
    fn the_chosen_id_is_free_as_a_uid_and_a_gid_on_both_sides() {
        let primary = users(&[], &[(1002, "a")], &[(1003, "b")]);
        let secondary = users(&[], &[(1004, "c")], &[(1005, "d")]);
        let id = choose_shared_id(&primary, &secondary);
        for taken in [1002, 1003, 1004, 1005] {
            assert_ne!(id, taken, "{id} is spent somewhere");
        }
        // 1000 is free everywhere, and a system account holding 1002 says
        // nothing about shared-home data — only accounts and home owners
        // raise the floor, so the lowest genuinely free number is correct.
        assert_eq!(id, 1000);

        // With 1000 and 1001 also spent, it walks past them.
        let primary = users(&[], &[(1000, "a"), (1002, "c")], &[(1001, "b")]);
        assert_eq!(choose_shared_id(&primary, &secondary), 1003);
    }

    /// A home whose account is gone on both boxes is invisible to the account
    /// tables. Handing its number out would give its files to somebody new.
    #[test]
    fn an_orphaned_home_still_reserves_its_number() {
        let mut primary = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        primary.home_owners = vec![(1004, 1004), (1042, 1042)];
        let secondary = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        let id = choose_shared_id(&primary, &secondary);
        assert!(id > 1042, "must clear the orphaned home's 1042, got {id}");
    }

    /// Root is squashed to nobody (65534) on EFS, so one file owned by it must
    /// not push every future account past 65535.
    #[test]
    fn the_squash_owner_does_not_drag_the_floor_up() {
        let mut primary = users(&[acct("jsmith", 1004, 1004)], &[(65534, "nobody")], &[]);
        primary.home_owners = vec![(65534, 65534)];
        let secondary = users(&[acct("jsmith", 1004, 1004)], &[], &[]);
        let id = choose_shared_id(&primary, &secondary);
        assert_eq!(id, 1005, "should sit above the real accounts, not the squash");
    }

    #[test]
    fn a_pair_with_no_accounts_starts_at_the_first_user_id() {
        assert_eq!(
            choose_shared_id(&BastionUsers::default(), &BastionUsers::default()),
            1000
        );
    }

    #[test]
    fn parse_dump_reads_home_owners() {
        let u = parse_dump("HOMEOWN 1011 1011\nHOMEOWN 1042 1043\nHOMEOWN bad\n");
        assert_eq!(u.home_owners, vec![(1011, 1011), (1042, 1043)]);
    }







}
