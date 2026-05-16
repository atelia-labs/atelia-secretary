## 2025-02-27 - [Fix overly permissive permissions on store files] (entry recorded: 2026-05-11)
**Vulnerability:** Newly created store snapshot files and ledger.json were created with default umask permissions during atomic writes instead of restrictive `0o600` permissions.
**Learning:** In Rust, `fs::File::create` or `OpenOptions::new().write(true).create_new(true).open()` without explicit `mode(0o600)` on unix can result in files with broader than intended read access.
**Prevention:** Always use `std::os::unix::fs::OpenOptionsExt` to set `mode(0o600)` when using `OpenOptions` to create new sensitive files.
## 2026-05-11 - [Restrictive Auth Token Directory Creation]
**Vulnerability:** The directory storing local auth tokens was created with default umask permissions (`std::fs::create_dir_all`) rather than restrictive permissions, potentially exposing the auth token directory.
**Learning:** Even if the token file itself is strictly permissioned, the parent directory must also restrict access to prevent traversal and directory listing attacks for sensitive items.
**Prevention:** Use `std::fs::DirBuilder::new().recursive(true).mode(0o700)` on Unix platforms when creating directories intended to hold secrets or sensitive configuration.
