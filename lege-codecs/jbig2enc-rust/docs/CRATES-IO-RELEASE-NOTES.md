# Release notes on crates.io — checked 2026-11-02

Crates.io does not currently provide a dedicated author-entered per-version
release-notes field or a Cargo changelog field. The
[published metadata protocol](https://doc.rust-lang.org/cargo/reference/registry-web-api.html#publish)
includes README content and repository/documentation links, but no release-note
payload. The proposed
[changelog URL field, RFC 3779](https://github.com/rust-lang/rfcs/pull/3779), remains
open and unmerged. The earlier
[RFC 2129](https://github.com/rust-lang/rfcs/pull/2129) was closed without merging;
older closed crates.io feature requests point to that discussion and do not
mean the feature shipped.

The low-friction approach for 0.6 is:

1. Keep a short “since 0.5.4” summary in README.md. Cargo uploads it and
   [crates.io renders it on the crate page](https://doc.rust-lang.org/cargo/reference/manifest.html#the-readme-field).
2. Keep the fuller versioned CHANGELOG.md in the repository and package. Link
   it from the README using an absolute repository URL. Packaging a changelog
   does not create a separate release-note tab automatically.
3. Optionally paste the same text into a GitHub Release when the release is
   actually made. No release automation or new dependency is needed for this.

A draft 0.6.0 entry and README summary are now present, explicitly marked
unreleased. The package include list includes CHANGELOG.md. Nothing was
published, tagged or committed by this pass. Before publishing, review the draft,
finalize the date/status and any compatibility notes, then publish through the
normal release process. This document does not add an approval gate.

The official
[registry index](https://github.com/rust-lang/crates.io-index/blob/master/jb/ig/jbig2enc-rust)
contained 0.5.0, 0.5.1, 0.5.3 and 0.5.4 when checked; 0.5.4 was the last
published version. The draft therefore describes changes since 0.5.4.

The crates.io team's
[July 2026 update](https://blog.rust-lang.org/2026/07/13/crates-io-development-update/)
describes published-source browsing and a planned version diff viewer. Those
help inspect code changes; they are separate from maintainer-written release
notes.
