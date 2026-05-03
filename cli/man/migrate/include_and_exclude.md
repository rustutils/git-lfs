You can have `git lfs migrate` convert only files whose pathspec
matches the `--include` glob patterns and does not match the
`--exclude` glob patterns, either to reduce total migration time or
to migrate part of your repo. Multiple patterns may be given using
commas as delimiters.

Pattern matching is functionally equivalent to the
`.gitattributes` format. In addition to simple file extension
matches (e.g. `*.gif`), patterns may also specify directory paths,
in which case the `path/**` form may be used to match recursively.

Note that this form of pattern matching for `--include` /
`--exclude` is unique to `git lfs migrate`. Other commands which
also take these options (such as `git lfs ls-files`) use the
gitignore(5) form of pattern matching instead.
