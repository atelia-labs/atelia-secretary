## 2025-02-27 - [Fix overly permissive permissions on store files]
**Vulnerability:** Newly created store snapshot files and ledger.json were created with default umask permissions during atomic writes instead of restrictive `0o600` permissions.
**Learning:** In Rust, `fs::File::create` or `OpenOptions::new().write(true).create_new(true).open()` without explicit `mode(0o600)` on unix can result in files with broader than intended read access.
**Prevention:** Always use `std::os::unix::fs::OpenOptionsExt` to set `mode(0o600)` when using `OpenOptions` to create new sensitive files.
