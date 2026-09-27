# Security Policy - `wyrd-greet`

`wyrd-greet` is a standalone Wayland session locker (`ext-session-lock-v1` + PAM) and `greetd` display manager greeter in the Wyrd ecosystem. Because it directly mediates authentication boundaries, it is maintained and self-reviewed as a separate security-critical project outside `wyrd-shell`.

## Supported Versions

| Version | Supported |
| :--- | :--- |
| `0.1.x` | Yes |

## Threat Model & Security Invariants

1. **End-to-End Secret Zeroization (`Zeroize`)**:
   - Interactive password buffers (`AppState::password`) are wrapped in `zeroize::Zeroizing<String>` preallocated to `MAX_PASSWORD_BYTES` (256 bytes) with a strict byte-length check (`self.password.len() + c.len_utf8() <= MAX_PASSWORD_BYTES`) to prevent heap reallocations from leaving stale copies in freed memory.
   - Temporary UTF-8 keystroke buffers returned by `xkb_state.key_get_utf8(key)` are immediately wiped via `utf8.zeroize()` on every key press.
   - Worker-thread PAM copies, intermediate `CString` allocations in `pam_conversation_fn`, `GreetdRequest::PostAuthMessageResponse` structs (`Drop` + explicit `zeroize_sensitive`), and serialized JSON byte buffers (`Vec<u8>`) sent over `$GREETD_SOCK` are explicitly zeroed via `zeroize::Zeroize` immediately after use.
   - Partial `PamResponse` allocations on conversation errors are wiped with `write_bytes(..., 0)` before `free()`.

2. **Hardened PAM FFI & Account Policy Enforcement (`src/pam.rs`)**:
   - Every `unsafe` FFI call into `libpam` (`pam_start`, `pam_authenticate`, `pam_acct_mgmt`, `pam_end`, and `pam_conversation_fn`) documents its exact memory safety invariants.
   - `pam_acct_mgmt(pamh, 0)` is always invoked after a successful `pam_authenticate(pamh, 0)` so expired passwords, locked accounts, and time-of-day access policies are strictly enforced.

3. **Compositor Watchdog & Non-Blocking Session Lock (`ext-session-lock-v1`)**:
   - `ext_session_lock_manager_v1::lock()` is issued at process startup before font enumeration or disk I/O.
   - System font scanning (`RenderContext::new`) and PAM/greetd authentication (`pam::authenticate_user` / `GreetdClient`) execute on isolated worker threads so slow PAM modules (`pam_faillock`, LDAP/AD, fingerprint readers) never block the Wayland protocol dispatch loop or trigger Hyprland's `misc:lockdead_screen_delay` watchdog.
   - `ext_session_lock_v1::unlock_and_destroy()` is called if and only if `pam_authenticate` AND `pam_acct_mgmt` both return `PAM_SUCCESS`.

4. **Shared Engine Dependencies (`wyrd-engine` `v0.1.0`)**:
   - In `v0.1.0`, `wyrd-engine` links `wasmtime` and `zbus` as shared crate dependencies of the core engine, though `wyrd-greet` never instantiates the WASM runtime or D-Bus module host. Isolating `wasmtime` and `zbus` behind optional Cargo feature flags in `wyrd-engine` is scheduled for `v0.2.0`.

## Reporting a Vulnerability

Please report security vulnerabilities privately via GitHub Security Advisories at <https://github.com/xximxxhatedxx/wyrd-greet/security/advisories/new> or by emailing `ghostr210305@gmail.com`. Do not open public issues for undisclosed authentication or session-lock bypass bugs.
