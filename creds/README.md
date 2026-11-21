# git-lfs-creds

Credential helpers for Git LFS, built on `git credential`.

Rather than keeping its own credential store, Git LFS uses the one Git is
already configured with, such as osxkeychain, libsecret, or Git Credential
Manager.

Each credential source implements the `Helper` trait, and a `HelperChain` tries
them in order. The Git LFS CLI uses `.netrc`, an in-memory cache, an askpass
program, and then `git credential`. Approved and rejected credentials go to the
first helper that handles them.

SSH remotes work differently. Instead of asking for credentials, the client
runs `git-lfs-authenticate` on the server over SSH, which returns a short-lived
HTTPS token. The token is cached until it expires.
