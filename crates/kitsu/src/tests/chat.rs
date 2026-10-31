use super::*;

#[test]
fn cwd_grant_gives_yes_runs_read_write_and_keeps_deny_by_default() {
    // --yes: read+write the launch dir, interactive or not.
    assert_eq!(
        cwd_grant(true, false),
        CwdGrant {
            read: true,
            write: true
        }
    );
    assert_eq!(
        cwd_grant(true, true),
        CwdGrant {
            read: true,
            write: true
        }
    );
    // Interactive without --yes: read+write the launch dir too (writes
    // inside it; Mode::Ask still prompts for writes outside it).
    assert_eq!(
        cwd_grant(false, true),
        CwdGrant {
            read: true,
            write: true
        }
    );
    // Non-interactive without --yes: nothing — deny-by-default stands.
    assert_eq!(
        cwd_grant(false, false),
        CwdGrant {
            read: false,
            write: false
        }
    );
}
