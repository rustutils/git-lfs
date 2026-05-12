`.lfsconfig` at the repository root uses the same format as `.git/config`. Only a restricted set of keys is honored here (the others are silently ignored), for security: a `.lfsconfig` from an untrusted clone shouldn't be able to override credential helpers or arbitrary Git config.

Allowed keys:

- `lfs.allowincompletepush`
- `lfs.fetchexclude`
- `lfs.fetchinclude`
- `lfs.gitprotocol`
- `lfs.locksverify`
- `lfs.pushurl`
- `lfs.skipdownloaderrors`
- `lfs.url`
- `lfs.<url>.access`
- `remote.<name>.lfsurl`
