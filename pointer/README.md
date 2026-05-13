# git-lfs-pointer

Parse and encode Git LFS pointer files.

A pointer is a small UTF-8 text file that stands in for a large file
in a git repository. It carries the file's SHA-256 OID, its size, and
an optional list of extension records. This crate handles parsing and
encoding of that format, with no I/O, no network, and no git
dependency.

The format is a sorted sequence of `key value` lines: the `version`
URL always first, then optional extension records sorted by
single-digit priority, then the `oid` and `size` lines. The whole
file must be under 1024 bytes; see the [spec](../docs/spec.md)
for the full grammar.

Parsing is permissive (CRLF line endings, trailing whitespace,
unsorted extensions, and older version URLs are all accepted), but
encoding always emits the canonical form. Each parsed pointer
carries a `canonical` flag so callers like the smudge filter can
pass the original bytes through verbatim when they already match;
re-encoding a non-canonical pointer would change its git blob hash.

Parse errors split into "not a pointer" (input bears no LFS markers
at all; callers should treat the bytes as opaque content) and
"malformed" (input has pointer shape but invalid contents; callers
should surface the error).
