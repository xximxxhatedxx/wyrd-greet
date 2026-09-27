# Changelog

All notable changes to `wyrd-greet` will be documented in this file.

## [0.1.0] - 2026-09-27

### Added
- Initial standalone release (`v0.1.0`) extracted into its own repository in the Wyrd ecosystem.
- Full `wyrd-engine` `WidgetTree`, `StyleConfig`, and 60 FPS `Animator` UI with `chrono` clock/date readout and active theme synchronization (`aetheria`, `catppuccin`, `tokyo-night`, `gruvbox`, `nord`, `dynamic`).
- Parallel background font-system initialization and off-thread PAM/`greetd` authentication to eliminate compositor lockdead watchdog false positives (`misc:lockdead_screen_delay`).
- End-to-end password and JSON payload zeroization (`Zeroize`) in `greetd.rs` and `pam.rs`, plus `pam_acct_mgmt` verification and unit tests for `pam_conversation_fn`.
