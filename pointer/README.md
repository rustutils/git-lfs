# git-lfs-pointer

Parse and encode Git LFS pointer files.

A pointer is a small text file that stands in for a large file in a Git
repository. It records the file's SHA-256 OID, its size, and optionally a list
of extensions. This crate parses and encodes that format, with no I/O and no
Git dependency. The [spec](../docs/spec.md) has the full grammar.

Parsing is lenient: CRLF line endings, whitespace at the start or end,
unsorted extensions, and older version URLs are accepted. Encoding always
produces the canonical form. Each parsed pointer records whether its input was
canonical, since re-encoding a non-canonical pointer changes its Git blob hash.

Parse errors distinguish input that isn't a pointer at all, which callers
should treat as ordinary content, from a malformed pointer, which callers
should report.
