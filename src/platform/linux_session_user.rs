use super::{get_values_of_seat0, log, CMD_LOGINCTL};
use std::process::Command;

pub(super) fn is_prelogin(username: &str) -> bool {
    if username.is_empty() {
        return true;
    }
    let session = get_values_of_seat0(&[0]);
    if let Some(sid) = session.first().filter(|sid| !sid.is_empty()) {
        match Command::new(&*CMD_LOGINCTL)
            .args(["show-session", "-p", "Class", "-p", "Name", sid])
            .output()
        {
            Ok(output) if output.status.success() => {
                if let Some(prelogin) =
                    session_prelogin(&String::from_utf8_lossy(&output.stdout), username)
                {
                    return prelogin;
                }
            }
            Ok(output) => log::debug!("Failed to query login session {sid}: {}", output.status),
            Err(err) => log::debug!("Failed to query login session {sid}: {err}"),
        }
    }

    match Command::new("getent").args(["passwd", username]).output() {
        Ok(output) if output.status.success() => {
            passwd_prelogin(&String::from_utf8_lossy(&output.stdout), username)
        }
        Ok(output) => {
            log::debug!("Failed to query active user account: {}", output.status);
            false
        }
        Err(err) => {
            log::debug!("Failed to query active user account: {err}");
            false
        }
    }
}

fn session_prelogin(properties: &str, username: &str) -> Option<bool> {
    let property = |key| properties.lines().find_map(|line| line.strip_prefix(key));
    // A session switch must not apply another user's class to a cached username.
    if username.is_empty() || property("Name=") != Some(username) {
        return None;
    }
    match property("Class=")? {
        "greeter" => Some(true),
        "user" | "user-early" | "user-light" | "user-early-light" => Some(false),
        _ => None,
    }
}

fn passwd_prelogin(passwd: &str, username: &str) -> bool {
    passwd.lines().any(|line| {
        let mut fields = line.split(':');
        fields.next() == Some(username)
            && matches!(fields.nth(5), Some("/bin/false" | "/usr/sbin/nologin"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_session_is_logged_in_regardless_of_account_shell() {
        for class in ["user", "user-early", "user-light", "user-early-light"] {
            assert_eq!(
                session_prelogin(&format!("Name=alice\nClass={class}\n"), "alice"),
                Some(false)
            );
        }
    }

    #[test]
    fn greeter_stays_prelogin_even_with_an_interactive_shell() {
        assert_eq!(
            session_prelogin("Class=greeter\nName=gdm\n", "gdm"),
            Some(true)
        );
    }

    #[test]
    fn missing_unknown_or_different_session_uses_account_fallback() {
        for properties in [
            "",
            "Name=alice\n",
            "Class=unknown\nName=alice\n",
            "Class=user\nName=bob\n",
            "Class=greeter\nName=gdm\n",
        ] {
            assert_eq!(session_prelogin(properties, "alice"), None);
        }
    }

    #[test]
    fn shell_like_text_in_home_or_gecos_is_not_prelogin() {
        assert!(!passwd_prelogin(
            "alice:x:1000:1000:Uses /usr/sbin/nologin:/home/bin/false:/bin/bash\n",
            "alice"
        ));
        assert!(!passwd_prelogin(
            "alice:x:1000:1000::/home/alice:/bin/false-wrapper\n",
            "alice"
        ));
    }

    #[test]
    fn fallback_checks_only_the_requested_accounts_shell() {
        assert!(passwd_prelogin(
            "gdm:x:120:120::/var/lib/gdm:/bin/false\n",
            "gdm"
        ));
        assert!(passwd_prelogin(
            "sddm:x:121:121::/var/lib/sddm:/usr/sbin/nologin\n",
            "sddm"
        ));
        assert!(!passwd_prelogin(
            "daemon:x:1:1::/usr/sbin:/usr/sbin/nologin\nalice:x:1000:1000::/home/alice:/bin/bash\n",
            "alice"
        ));
    }
}
