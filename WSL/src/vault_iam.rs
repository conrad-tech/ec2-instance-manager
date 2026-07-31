//! "Vault IAM Access" — build the commands that create a Vault policy and an
//! AWS-auth role bound to an IAM role, and read the verdict back.
//!
//! The commands run on a bastion through the Scripts drip-feed, one line at a
//! time. Everything here is pure: validation, the step plan, and parsing the
//! result marker out of captured terminal text. The GUI owns the dialog and the
//! PTY plumbing.

use base64::Engine;

/// Marker echoed by the final step when the run achieved its goal.
pub const OK_MARKER: &str = "__VAULT_IAM_OK__";
/// Marker echoed by the final step when it did not.
pub const FAIL_MARKER: &str = "__VAULT_IAM_FAIL__";

/// Outcome of a run, read from the terminal after the steps finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The run achieved its goal: for a create, both objects read back; for a
    /// delete, neither does.
    Ok,
    /// The check ran and the goal was not met.
    Failed,
    /// Neither marker was found — the session died, scrolled away, or never
    /// reached the last step. Treated as a failure by the caller.
    Unknown,
}

/// Everything the dialog collects for one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaultIamRequest {
    /// Full IAM role ARN, used verbatim as `bound_iam_principal_arn`.
    pub iam_role_arn: String,
    /// Policy HCL, shipped base64-encoded so multi-line bodies survive.
    pub policy_body: String,
    /// Vault role name — the `auth/aws/role/<name>` path.
    pub role_name: String,
    /// Vault policy name, referenced by `policies="…"`.
    pub policy_name: String,
    pub vault_addr: String,
    /// Typed per run and never persisted anywhere.
    pub vault_token: String,
}

/// Role name from an IAM role ARN: the last segment after `role/`, so a role
/// with a path (`role/team/app-role`) yields `app-role`.
///
/// Returns `None` when the ARN isn't a role ARN, which is also what makes this
/// usable as a live "does this look right yet" check while the user types.
pub fn role_name_from_arn(arn: &str) -> Option<String> {
    let arn = arn.trim();
    let (_, after) = arn.split_once(":role/")?;
    let name = after.rsplit('/').next().unwrap_or_default().trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Characters allowed in a Vault role or policy name. Deliberately narrow:
/// these names are interpolated into a shell command line unquoted.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// An IAM role ARN this tool will accept.
///
/// Shape: `arn:<partition>:iam::<12-digit account>:role/<path?><name>`. The
/// partition is checked loosely so GovCloud and China ARNs work. Characters
/// that would break out of the double-quoted shell argument are rejected
/// outright rather than escaped — a legitimate ARN never contains them.
fn is_valid_role_arn(arn: &str) -> bool {
    let arn = arn.trim();
    if arn.chars().any(|c| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '$' | '\\' | ';' | '&' | '|')
    }) {
        return false;
    }
    let mut parts = arn.split(':');
    if parts.next() != Some("arn") {
        return false;
    }
    let partition = parts.next().unwrap_or_default();
    if !partition.starts_with("aws") {
        return false;
    }
    if parts.next() != Some("iam") {
        return false;
    }
    // Region is always empty for IAM.
    if parts.next() != Some("") {
        return false;
    }
    let account = parts.next().unwrap_or_default();
    if account.len() != 12 || !account.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let resource = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return false;
    }
    resource
        .strip_prefix("role/")
        .is_some_and(|rest| !rest.trim().is_empty() && !rest.ends_with('/'))
}

/// A Vault address safe to embed in the single-quoted export.
fn is_valid_vault_addr(addr: &str) -> bool {
    let addr = addr.trim();
    (addr.starts_with("http://") || addr.starts_with("https://"))
        && !addr.chars().any(|c| c.is_whitespace() || c == '\'')
}

/// Export the address and token, then wipe them off the screen.
///
/// The token is base64-encoded rather than written literally, and the export
/// carries a leading space so the preceding `HISTCONTROL=ignorespace` keeps it
/// out of the remote shell history. `clear` runs immediately after, before any
/// output worth reading, so the verification below stays visible.
fn connect_steps(vault_addr: &str, vault_token: &str) -> Vec<String> {
    let token_b64 = base64::engine::general_purpose::STANDARD.encode(vault_token.trim());
    vec![
        "export HISTCONTROL=ignorespace".to_string(),
        format!(
            " export VAULT_ADDR='{}'; \
             export VAULT_TOKEN=\"$(echo '{token_b64}' | base64 -d)\"; clear",
            vault_addr.trim()
        ),
    ]
}

/// Final step: run `ok_test` and echo the verdict marker.
///
/// The marker is assembled from `$v` at runtime. The shell echoes each command
/// before running it, so a literal `__VAULT_IAM_OK__` in the command line would
/// make [`parse_verdict`] match the echo instead of the output and report
/// success regardless of what Vault did.
fn verdict_step(ok_test: &str) -> String {
    format!("if {ok_test}; then v=OK; else v=FAIL; fi; echo \"__VAULT_IAM_${{v}}__\"")
}

/// Everything the delete dialog collects. The mirror image of
/// [`VaultIamRequest`]: no ARN and no policy body, because it removes the two
/// objects a create made rather than describing them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaultIamDeleteRequest {
    pub role_name: String,
    pub policy_name: String,
    pub vault_addr: String,
    /// Typed per run and never persisted anywhere.
    pub vault_token: String,
}

impl VaultIamDeleteRequest {
    /// Check every field, returning the message for the dialog's error line.
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_name(self.role_name.trim()) {
            return Err(
                "Vault role name must be letters, digits, '_', '.' or '-'.".to_string(),
            );
        }
        if !is_valid_name(self.policy_name.trim()) {
            return Err("Policy name must be letters, digits, '_', '.' or '-'.".to_string());
        }
        if !is_valid_vault_addr(&self.vault_addr) {
            return Err("Enter a VAULT_ADDR starting with http:// or https://.".to_string());
        }
        if self.vault_token.trim().is_empty() {
            return Err("Enter a VAULT_TOKEN.".to_string());
        }
        Ok(())
    }

    /// The shell commands to drip-feed to the bastion, in order.
    ///
    /// Call [`validate`](Self::validate) first. Deleting something that is
    /// already gone is not an error here: Vault's delete is idempotent, and the
    /// verdict checks the end state rather than the delete's exit code, so a
    /// half-finished earlier run still converges on OK.
    pub fn steps(&self) -> Vec<String> {
        let role = self.role_name.trim();
        let policy = self.policy_name.trim();
        let mut steps = connect_steps(&self.vault_addr, &self.vault_token);
        steps.push(format!("vault delete auth/aws/role/{role}"));
        steps.push(format!("vault policy delete {policy}"));
        // Shown to the user, so they can see the role is no longer listed.
        steps.push("vault list auth/aws/role".to_string());
        steps.push(verdict_step(&format!(
            "! vault read auth/aws/role/{role} >/dev/null 2>&1 && \
             ! vault policy read {policy} >/dev/null 2>&1"
        )));
        steps
    }
}

impl VaultIamRequest {
    /// Check every field, returning the message to show on the dialog's error
    /// line. Ordered so the user fixes the top of the form first.
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_role_arn(&self.iam_role_arn) {
            return Err(
                "Enter a full IAM role ARN, e.g. arn:aws:iam::123456789012:role/my-role."
                    .to_string(),
            );
        }
        if self.policy_body.trim().is_empty() {
            return Err("Enter the policy body.".to_string());
        }
        if !is_valid_name(self.role_name.trim()) {
            return Err(
                "Vault role name must be letters, digits, '_', '.' or '-'.".to_string()
            );
        }
        if !is_valid_name(self.policy_name.trim()) {
            return Err("Policy name must be letters, digits, '_', '.' or '-'.".to_string());
        }
        if !is_valid_vault_addr(&self.vault_addr) {
            return Err("Enter a VAULT_ADDR starting with http:// or https://.".to_string());
        }
        if self.vault_token.trim().is_empty() {
            return Err("Enter a VAULT_TOKEN.".to_string());
        }
        Ok(())
    }

    /// The shell commands to drip-feed to the bastion, in order.
    ///
    /// Call [`validate`](Self::validate) first — this assumes the fields are
    /// already safe to interpolate.
    ///
    /// Two things are base64-encoded rather than written literally:
    ///
    /// * the **policy body**, so multi-line HCL with quotes and braces survives
    ///   a transport that sends one line at a time (the same trick
    ///   `create_new_user.sh` uses), and
    /// * the **token**, so it never appears literally on the command line.
    ///   Combined with the leading space under `HISTCONTROL=ignorespace` it
    ///   stays out of the remote shell history, and the `clear` that follows
    ///   moves it off the visible screen before the verification output.
    pub fn steps(&self) -> Vec<String> {
        let role = self.role_name.trim();
        let policy = self.policy_name.trim();
        // Strip CR so a Windows (autocrlf) clipboard or file doesn't feed the
        // remote bash a CRLF policy body.
        let policy_b64 = base64::engine::general_purpose::STANDARD
            .encode(self.policy_body.replace('\r', ""));

        let mut steps = connect_steps(&self.vault_addr, &self.vault_token);
        steps.push(format!(
            "echo '{policy_b64}' | base64 -d | vault policy write {policy} -"
        ));
        steps.push(format!(
            "vault write auth/aws/role/{role} \
             bound_iam_principal_arn=\"{}\" \
             resolve_aws_unique_id=true \
             policies=\"{policy}\" \
             token_ttl=0s \
             token_max_ttl=24h \
             max_ttl=24h",
            self.iam_role_arn.trim()
        ));
        // Shown to the user — this is the "list it to make sure it got
        // created" step.
        steps.push(format!("vault policy read {policy}"));
        steps.push(format!("vault read auth/aws/role/{role}"));
        steps.push(verdict_step(&format!(
            "vault policy read {policy} >/dev/null 2>&1 && \
             vault read auth/aws/role/{role} >/dev/null 2>&1"
        )));
        steps
    }
}

/// Read the verdict out of captured terminal text.
///
/// `FAIL` is checked first so a screen holding both markers — a retry after a
/// failure, say — reports the failure rather than silently passing.
pub fn parse_verdict(screen: &str) -> Verdict {
    if screen.contains(FAIL_MARKER) {
        Verdict::Failed
    } else if screen.contains(OK_MARKER) {
        Verdict::Ok
    } else {
        Verdict::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> VaultIamRequest {
        VaultIamRequest {
            iam_role_arn: "arn:aws:iam::123456789012:role/my-role".to_string(),
            policy_body: "path \"ctt/*\" {\n  capabilities = [\"read\", \"write\", \"list\"]\n}\n"
                .to_string(),
            role_name: "my-role".to_string(),
            policy_name: "my-role".to_string(),
            vault_addr: "https://vault.example.com:8200".to_string(),
            vault_token: "hvs.exampletoken".to_string(),
        }
    }

    #[test]
    fn role_name_is_taken_from_the_arn() {
        assert_eq!(
            role_name_from_arn("arn:aws:iam::123456789012:role/my-role").as_deref(),
            Some("my-role")
        );
    }

    #[test]
    fn role_name_ignores_an_iam_path() {
        assert_eq!(
            role_name_from_arn("arn:aws:iam::123456789012:role/team/app-role").as_deref(),
            Some("app-role")
        );
    }

    #[test]
    fn role_name_is_none_for_a_non_role_arn() {
        assert_eq!(role_name_from_arn("arn:aws:iam::123456789012:user/bob"), None);
        assert_eq!(role_name_from_arn("my-role"), None);
        assert_eq!(role_name_from_arn(""), None);
        assert_eq!(role_name_from_arn("arn:aws:iam::123456789012:role/"), None);
    }

    #[test]
    fn valid_arns_are_accepted() {
        assert!(is_valid_role_arn("arn:aws:iam::123456789012:role/my-role"));
        assert!(is_valid_role_arn("arn:aws:iam::123456789012:role/team/app-role"));
        assert!(is_valid_role_arn("  arn:aws:iam::123456789012:role/my-role  "));
        // GovCloud / China partitions.
        assert!(is_valid_role_arn("arn:aws-us-gov:iam::123456789012:role/r"));
        assert!(is_valid_role_arn("arn:aws-cn:iam::123456789012:role/r"));
    }

    #[test]
    fn malformed_arns_are_rejected() {
        assert!(!is_valid_role_arn("my-role"), "bare name");
        assert!(!is_valid_role_arn("arn:aws:iam::12345:role/r"), "short account");
        assert!(
            !is_valid_role_arn("arn:aws:iam::12345678901a:role/r"),
            "non-numeric account"
        );
        assert!(!is_valid_role_arn("arn:aws:iam::123456789012:user/bob"), "not a role");
        assert!(
            !is_valid_role_arn("arn:aws:iam:us-east-1:123456789012:role/r"),
            "IAM ARNs carry no region"
        );
        assert!(!is_valid_role_arn("arn:gcp:iam::123456789012:role/r"), "partition");
    }

    #[test]
    fn arns_that_could_break_out_of_the_shell_quoting_are_rejected() {
        // The ARN is interpolated into a double-quoted argument.
        assert!(!is_valid_role_arn("arn:aws:iam::123456789012:role/a\"; rm -rf /; \""));
        assert!(!is_valid_role_arn("arn:aws:iam::123456789012:role/$(whoami)"));
        assert!(!is_valid_role_arn("arn:aws:iam::123456789012:role/a b"));
        assert!(!is_valid_role_arn("arn:aws:iam::123456789012:role/a`id`"));
    }

    #[test]
    fn a_complete_request_validates() {
        assert_eq!(request().validate(), Ok(()));
    }

    #[test]
    fn validation_rejects_each_missing_field() {
        let cases: Vec<(&str, VaultIamRequest)> = vec![
            ("ARN", VaultIamRequest { iam_role_arn: "nope".into(), ..request() }),
            ("policy body", VaultIamRequest { policy_body: "   ".into(), ..request() }),
            ("role name", VaultIamRequest { role_name: String::new(), ..request() }),
            ("policy name", VaultIamRequest { policy_name: String::new(), ..request() }),
            ("vault addr", VaultIamRequest { vault_addr: String::new(), ..request() }),
            ("token", VaultIamRequest { vault_token: "  ".into(), ..request() }),
        ];
        for (field, req) in cases {
            assert!(req.validate().is_err(), "{field} should be required");
        }
    }

    #[test]
    fn validation_rejects_unsafe_names_and_addresses() {
        let bad_name = VaultIamRequest { role_name: "my role;rm -rf /".into(), ..request() };
        assert!(bad_name.validate().is_err());

        let bad_policy = VaultIamRequest { policy_name: "a'b".into(), ..request() };
        assert!(bad_policy.validate().is_err());

        // Must carry a scheme, and must not break the single-quoted export.
        let no_scheme = VaultIamRequest { vault_addr: "vault.example.com".into(), ..request() };
        assert!(no_scheme.validate().is_err());
        let quoted = VaultIamRequest {
            vault_addr: "https://a'; echo pwned; '".into(),
            ..request()
        };
        assert!(quoted.validate().is_err());
    }

    #[test]
    fn steps_export_the_address_and_decode_the_token() {
        let steps = request().steps();
        assert_eq!(steps[0], "export HISTCONTROL=ignorespace");
        assert!(
            steps[1].starts_with(' '),
            "the export must be history-suppressed"
        );
        assert!(steps[1].contains("VAULT_ADDR='https://vault.example.com:8200'"));
        assert!(
            !steps[1].contains("hvs.exampletoken"),
            "the token must never appear literally on the command line"
        );
        assert!(steps[1].ends_with("clear"), "wipe the token off screen");
    }

    #[test]
    fn the_encoded_token_decodes_back() {
        let steps = request().steps();
        let start = steps[1].find("echo '").expect("token echo") + "echo '".len();
        let end = start + steps[1][start..].find('\'').expect("closing quote");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&steps[1][start..end])
            .expect("valid base64");
        assert_eq!(String::from_utf8(decoded).unwrap(), "hvs.exampletoken");
    }

    #[test]
    fn the_encoded_policy_decodes_back_with_its_newlines() {
        let req = request();
        let steps = req.steps();
        let write = steps.iter().find(|s| s.contains("vault policy write")).unwrap();
        let start = write.find('\'').unwrap() + 1;
        let end = start + write[start..].find('\'').unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&write[start..end])
            .expect("valid base64");
        assert_eq!(String::from_utf8(decoded).unwrap(), req.policy_body);
        assert!(write.ends_with("| vault policy write my-role -"));
    }

    #[test]
    fn carriage_returns_are_stripped_from_the_policy() {
        // A policy pasted from Windows would otherwise reach bash as CRLF.
        let req = VaultIamRequest {
            policy_body: "path \"ctt/*\" {\r\n}\r\n".to_string(),
            ..request()
        };
        let steps = req.steps();
        let write = steps.iter().find(|s| s.contains("vault policy write")).unwrap();
        let start = write.find('\'').unwrap() + 1;
        let end = start + write[start..].find('\'').unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&write[start..end])
            .unwrap();
        let text = String::from_utf8(decoded).unwrap();
        assert!(!text.contains('\r'));
        assert_eq!(text, "path \"ctt/*\" {\n}\n");
    }

    #[test]
    fn the_role_write_matches_the_reference_command() {
        let steps = request().steps();
        let write = steps
            .iter()
            .find(|s| s.starts_with("vault write auth/aws/role/"))
            .expect("role write step");
        assert!(write.contains("vault write auth/aws/role/my-role"));
        assert!(write
            .contains("bound_iam_principal_arn=\"arn:aws:iam::123456789012:role/my-role\""));
        assert!(write.contains("resolve_aws_unique_id=true"));
        assert!(write.contains("policies=\"my-role\""));
        assert!(write.contains("token_ttl=0s"));
        assert!(write.contains("token_max_ttl=24h"));
        assert!(write.contains("max_ttl=24h"));
    }

    #[test]
    fn steps_read_both_objects_back_for_the_user_to_see() {
        let steps = request().steps();
        assert!(steps.iter().any(|s| s == "vault policy read my-role"));
        assert!(steps.iter().any(|s| s == "vault read auth/aws/role/my-role"));
    }

    #[test]
    fn the_verdict_step_does_not_contain_the_literal_markers() {
        // The shell echoes each command before running it. If the command line
        // held the marker, scanning the screen would match the echo and report
        // success no matter what Vault did.
        let steps = request().steps();
        let check = steps.last().unwrap();
        assert!(!check.contains(OK_MARKER), "would self-match: {check}");
        assert!(!check.contains(FAIL_MARKER), "would self-match: {check}");
        assert!(check.contains("__VAULT_IAM_${v}__"));
    }

    #[test]
    fn steps_never_elevate() {
        // Vault authenticates by token, not by OS user.
        assert!(!request().steps().iter().any(|s| s.contains("sudo")));
    }

    fn delete_request() -> VaultIamDeleteRequest {
        VaultIamDeleteRequest {
            role_name: "my-role".to_string(),
            policy_name: "my-role".to_string(),
            vault_addr: "https://vault.example.com:8200".to_string(),
            vault_token: "hvs.exampletoken".to_string(),
        }
    }

    #[test]
    fn a_complete_delete_request_validates() {
        assert_eq!(delete_request().validate(), Ok(()));
    }

    #[test]
    fn delete_validation_rejects_each_missing_field() {
        let cases: Vec<(&str, VaultIamDeleteRequest)> = vec![
            ("role name", VaultIamDeleteRequest { role_name: String::new(), ..delete_request() }),
            ("policy name", VaultIamDeleteRequest { policy_name: "  ".into(), ..delete_request() }),
            ("vault addr", VaultIamDeleteRequest { vault_addr: String::new(), ..delete_request() }),
            ("token", VaultIamDeleteRequest { vault_token: "  ".into(), ..delete_request() }),
        ];
        for (field, req) in cases {
            assert!(req.validate().is_err(), "{field} should be required");
        }
    }

    #[test]
    fn delete_validation_rejects_unsafe_names() {
        let bad = VaultIamDeleteRequest {
            role_name: "my-role; vault delete secret/prod".into(),
            ..delete_request()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn delete_removes_both_the_role_and_the_policy() {
        let steps = delete_request().steps();
        assert!(steps.iter().any(|s| s == "vault delete auth/aws/role/my-role"));
        assert!(steps.iter().any(|s| s == "vault policy delete my-role"));
    }

    #[test]
    fn delete_lists_the_remaining_roles() {
        assert!(delete_request()
            .steps()
            .iter()
            .any(|s| s == "vault list auth/aws/role"));
    }

    #[test]
    fn delete_shares_the_connect_prelude_with_create() {
        // Same token hygiene on both paths — history-suppressed, encoded,
        // cleared off screen.
        let delete = delete_request().steps();
        let create = request().steps();
        assert_eq!(delete[0], create[0]);
        assert_eq!(delete[1], create[1]);
        assert!(!delete[1].contains("hvs.exampletoken"));
    }

    #[test]
    fn delete_succeeds_only_when_both_objects_are_gone() {
        let check = delete_request().steps().last().unwrap().clone();
        // Negated reads: OK means neither object can be read back.
        assert!(check.contains("! vault read auth/aws/role/my-role"));
        assert!(check.contains("! vault policy read my-role"));
        assert!(check.contains("&&"), "both must be absent, not either");
    }

    #[test]
    fn the_delete_verdict_step_does_not_contain_the_literal_markers() {
        let check = delete_request().steps().last().unwrap().clone();
        assert!(!check.contains(OK_MARKER), "would self-match: {check}");
        assert!(!check.contains(FAIL_MARKER), "would self-match: {check}");
    }

    #[test]
    fn delete_never_elevates() {
        assert!(!delete_request().steps().iter().any(|s| s.contains("sudo")));
    }

    #[test]
    fn delete_undoes_exactly_what_create_made() {
        // The two paths must agree on the object names, or a delete would
        // leave the role behind and a re-create would collide with it.
        let create = request();
        let delete = VaultIamDeleteRequest {
            role_name: create.role_name.clone(),
            policy_name: create.policy_name.clone(),
            vault_addr: create.vault_addr.clone(),
            vault_token: create.vault_token.clone(),
        };
        let created_role = format!("vault write auth/aws/role/{}", create.role_name);
        assert!(create.steps().iter().any(|s| s.starts_with(&created_role)));
        assert!(delete
            .steps()
            .iter()
            .any(|s| *s == format!("vault delete auth/aws/role/{}", create.role_name)));
        assert!(delete
            .steps()
            .iter()
            .any(|s| *s == format!("vault policy delete {}", create.policy_name)));
    }

    #[test]
    fn verdict_is_read_from_the_marker() {
        assert_eq!(
            parse_verdict("Success! Data written\n__VAULT_IAM_OK__\n$ "),
            Verdict::Ok
        );
        assert_eq!(parse_verdict("no such policy\n__VAULT_IAM_FAIL__\n"), Verdict::Failed);
    }

    #[test]
    fn a_missing_marker_is_unknown_not_success() {
        assert_eq!(parse_verdict(""), Verdict::Unknown);
        assert_eq!(parse_verdict("Success! Data written to: auth/aws/role/x"), Verdict::Unknown);
    }

    #[test]
    fn failure_wins_when_both_markers_are_on_screen() {
        // e.g. a successful run still visible above a failed retry.
        assert_eq!(
            parse_verdict("__VAULT_IAM_OK__\n…\n__VAULT_IAM_FAIL__\n"),
            Verdict::Failed
        );
    }
}
