use regex::Regex;
use std::sync::OnceLock;

fn revert_subject_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Matches the standard `git revert` generated subject:
    //   Revert "Original subject line"
    RE.get_or_init(|| Regex::new(r#"^Revert\s+"(.+)"\s*$"#).unwrap())
}

fn revert_this_reverts_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Matches the body line `git revert` appends:
    //   This reverts commit <sha>.
    RE.get_or_init(|| Regex::new(r"This reverts commit ([0-9a-fA-F]{7,40})").unwrap())
}

fn incident_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // JIRA-style keys (ABC-123, INC-4821, BUG-12, ...) in group 1, and bare
    // issue refs (#123) in group 2. These need separate alternatives rather
    // than a shared `\b...\b` wrapper: `#` is a non-word character, so a
    // leading `\b` never matches directly before it (space -> `#` is a
    // non-word -> non-word transition, not a boundary), which would
    // silently drop every "#123" reference.
    RE.get_or_init(|| Regex::new(r"(?i)\b([A-Z]{2,10}-\d+)\b|(#\d+)\b").unwrap())
}

fn fix_subject_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(fix|fixes|fixed|hotfix|patch)\b").unwrap())
}

/// True if the commit message looks like a `git revert`-generated commit.
pub fn is_revert_message(subject: &str) -> bool {
    revert_subject_re().is_match(subject.trim())
}

/// Pulls the reverted commit hash out of a revert commit's full message body, if present.
pub fn extract_reverted_hash(full_message: &str) -> Option<String> {
    revert_this_reverts_re()
        .captures(full_message)
        .map(|c| c[1].to_string())
}

/// True if the commit subject reads like a bugfix (used for repeated-fix-file tracking).
pub fn is_fix_message(subject: &str) -> bool {
    fix_subject_re().is_match(subject)
}

/// Extracts incident/bug-number references from a commit message. Deduped, order-preserved.
pub fn extract_incident_refs(message: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in incident_ref_re().captures_iter(message) {
        let matched = cap.get(1).or_else(|| cap.get(2)).unwrap().as_str();
        let m = matched.to_uppercase();
        if seen.insert(m.clone()) {
            out.push(m);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_revert_subject() {
        assert!(is_revert_message(r#"Revert "Add flaky retry logic""#));
        assert!(!is_revert_message("Add retry logic"));
    }

    #[test]
    fn extracts_reverted_hash() {
        let body = "Revert \"Add flaky retry logic\"\n\nThis reverts commit abc1234def5678900000000000000000000000.\n";
        assert_eq!(
            extract_reverted_hash(body).as_deref(),
            Some("abc1234def5678900000000000000000000000")
        );
    }

    #[test]
    fn detects_fix_message() {
        assert!(is_fix_message("Fix null pointer in parser"));
        assert!(is_fix_message("hotfix: race condition"));
        assert!(!is_fix_message("Add new feature"));
    }

    #[test]
    fn extracts_incident_refs() {
        let refs = extract_incident_refs("Fix crash (INC-4821), see also #987 and JIRA-12");
        assert_eq!(refs, vec!["INC-4821", "#987", "JIRA-12"]);
    }

    #[test]
    fn dedupes_incident_refs() {
        let refs = extract_incident_refs("relates to #1 and also #1 again");
        assert_eq!(refs, vec!["#1"]);
    }
}
