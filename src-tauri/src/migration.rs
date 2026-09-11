#[cfg(any(target_os = "macos", target_os = "windows"))]
use crate::headless_process;
use serde_json::Value;
use std::{env, fs, path::PathBuf, sync::Mutex};

pub struct MigrationState(pub Mutex<Option<PendingMigration>>);

pub struct PendingMigration {
    destination: PathBuf,
    marker: PathBuf,
    completed: bool,
    remove_destination_on_rollback: bool,
}

impl PendingMigration {
    pub fn commit(mut self) -> Result<(), String> {
        disable_legacy_service()?;
        // Once the old service is disabled, never delete a credential that
        // the new app has already proven it can use. If writing the marker
        // fails, the next launch safely retries only the idempotent cleanup.
        self.completed = true;
        fs::write(
            &self.marker,
            b"{\"credential_imported\":true,\"legacy_service_disabled\":true}\n",
        )
        .map_err(|error| format!("could not record completion of the legacy migration: {error}"))
    }

    pub fn rollback(self) {
        if self.remove_destination_on_rollback {
            let _ = fs::remove_file(&self.destination);
        }
    }
}

impl Drop for PendingMigration {
    fn drop(&mut self) {
        if !self.completed && self.remove_destination_on_rollback {
            let _ = fs::remove_file(&self.destination);
        }
    }
}

pub fn import_legacy_credential(app_data_dir: &std::path::Path) -> Option<PendingMigration> {
    let marker = app_data_dir.join("legacy-migration.json");
    if marker.exists() {
        return None;
    }
    let destination = app_data_dir.join("worker/ssh-agent-bridge.json");
    let source = legacy_credential_path()?;
    let bytes = fs::read(source).ok()?;
    if !valid_credential(&bytes) {
        return None;
    }
    if let Ok(existing) = fs::read(&destination) {
        if valid_credential(&existing) {
            let remove_destination_on_rollback = existing == bytes;
            return Some(PendingMigration {
                destination,
                marker,
                completed: false,
                // An identical file is an interrupted import. A different,
                // valid file belongs to the new app and must survive rollback.
                remove_destination_on_rollback,
            });
        }
    }
    fs::create_dir_all(destination.parent()?).ok()?;
    let temporary = destination.with_extension("tmp");
    fs::write(&temporary, bytes).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).ok()?;
    }
    fs::rename(temporary, &destination).ok()?;
    Some(PendingMigration {
        destination,
        marker,
        completed: false,
        remove_destination_on_rollback: true,
    })
}

fn valid_credential(bytes: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let valid_base_url = value
        .get("baseUrl")
        .and_then(Value::as_str)
        .and_then(|item| url::Url::parse(item).ok())
        .is_some_and(|item| matches!(item.scheme(), "http" | "https"));
    let valid_bridge_id = value
        .get("bridgeId")
        .and_then(Value::as_str)
        .is_some_and(is_uuid);
    let valid_session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .is_some_and(|item| item.len() == 64 && item.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let valid_expiration = match value.get("expiresAt") {
        None => true,
        Some(Value::String(item)) => chrono::DateTime::parse_from_rfc3339(item).is_ok(),
        Some(_) => false,
    };
    valid_base_url && valid_bridge_id && valid_session_id && valid_expiration
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn legacy_credential_path() -> Option<PathBuf> {
    let home =
        env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)?;
    if cfg!(windows) {
        let base = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"));
        Some(base.join("Mullion/ssh-agent-bridge.json"))
    } else {
        let base = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"));
        Some(base.join("mullion/ssh-agent-bridge.json"))
    }
}

#[cfg(target_os = "macos")]
fn disable_legacy_service() -> Result<(), String> {
    let label = "de.s3ntin3l8.mullion-helper";
    if let Ok(output) = headless_process::command("id").arg("-u").output() {
        if let Ok(uid) = String::from_utf8(output.stdout) {
            let domain = format!("gui/{}", uid.trim());
            let _ = headless_process::command("launchctl")
                .args(["bootout", &format!("{domain}/{label}")])
                .status();
            let status = headless_process::command("launchctl")
                .args(["disable", &format!("{domain}/{label}")])
                .status()
                .map_err(|error| format!("could not disable the legacy launchd job: {error}"))?;
            if status.success() {
                return Ok(());
            }
        }
    }
    Err("could not disable the legacy launchd job; the old helper remains active".into())
}

#[cfg(target_os = "windows")]
fn disable_legacy_service() -> Result<(), String> {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let query = headless_process::command("reg")
        .args(["query", key, "/v", "MullionHelper"])
        .status();
    if query.is_ok_and(|status| status.success()) {
        let deleted = headless_process::command("reg")
            .args(["delete", key, "/v", "MullionHelper", "/f"])
            .status()
            .map_err(|error| format!("could not disable the legacy Run entry: {error}"))?;
        if !deleted.success() {
            return Err(
                "could not disable the legacy Run entry; the old helper remains active".into(),
            );
        }
    }

    // The retired SEA and this Tauri app intentionally share the
    // mullion-helper.exe product name. Never use taskkill /IM here: it can
    // select this process and tear down a successful migration before the
    // credential is committed. Match the legacy install path and exclude
    // our PID explicitly.
    let local_app_data = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "LOCALAPPDATA is unavailable; cannot stop the legacy helper".to_owned())?;
    let legacy_executable = local_app_data.join("Mullion/mullion-helper.exe");
    let script = r#"
$legacyPath = [IO.Path]::GetFullPath($args[0])
$currentPid = [uint32]$args[1]
$targets = @(Get-CimInstance Win32_Process -Filter "Name = 'mullion-helper.exe'" |
  Where-Object {
    $_.ProcessId -ne $currentPid -and
    $_.ExecutablePath -and
    [IO.Path]::GetFullPath($_.ExecutablePath) -eq $legacyPath
  })
foreach ($target in $targets) {
  $result = Invoke-CimMethod -InputObject $target -MethodName Terminate
  if ($result.ReturnValue -ne 0) { exit 1 }
}
"#;
    let status = headless_process::command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .arg(&legacy_executable)
        .arg(std::process::id().to_string())
        .status()
        .map_err(|error| format!("could not stop the legacy helper process: {error}"))?;
    if !status.success() {
        return Err(
            "could not stop the legacy helper process; the imported credential was rolled back"
                .into(),
        );
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn disable_legacy_service() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_incomplete_credentials() {
        assert!(!valid_credential(br#"{"baseUrl":"https://example.com"}"#));
    }
    #[test]
    fn accepts_legacy_shape() {
        assert!(valid_credential(
            br#"{"baseUrl":"https://example.com","bridgeId":"123e4567-e89b-12d3-a456-426614174000","sessionId":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#
        ));
    }
    #[test]
    fn rejects_invalid_base_url() {
        assert!(!valid_credential(
            br#"{"baseUrl":"ftp://example.com","bridgeId":"123e4567-e89b-12d3-a456-426614174000","sessionId":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#
        ));
    }
    #[test]
    fn rejects_invalid_bridge_id() {
        assert!(!valid_credential(
            br#"{"baseUrl":"https://example.com","bridgeId":"bridge_1","sessionId":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#
        ));
    }
    #[test]
    fn rejects_invalid_session_id() {
        assert!(!valid_credential(
            br#"{"baseUrl":"https://example.com","bridgeId":"123e4567-e89b-12d3-a456-426614174000","sessionId":"secret"}"#
        ));
    }
    #[test]
    fn rejects_invalid_expiration() {
        assert!(!valid_credential(
            br#"{"baseUrl":"https://example.com","bridgeId":"123e4567-e89b-12d3-a456-426614174000","sessionId":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","expiresAt":"not-a-date"}"#
        ));
        assert!(!valid_credential(
            br#"{"baseUrl":"https://example.com","bridgeId":"123e4567-e89b-12d3-a456-426614174000","sessionId":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","expiresAt":123}"#
        ));
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_shutdown_targets_the_legacy_path_not_the_image_name() {
        let source = include_str!("migration.rs");
        assert!(source.contains("Mullion/mullion-helper.exe"));
        assert!(!source.contains(r#".args(["/IM""#));
        assert!(source.contains("$_.ProcessId -ne $currentPid"));
    }
}
