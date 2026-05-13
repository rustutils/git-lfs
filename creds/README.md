# git-lfs-creds

Credential helper bridge for Git LFS (git credential fill/approve/reject).

LFS endpoints are usually HTTPS, and HTTPS auth needs a username and
password. Rather than maintaining a separate credential store, this
crate defers to git's existing one: whatever the user has already
configured for their git remote (osxkeychain, libsecret, manager,
store, plain `cache`, …) is what LFS uses too.

A `Helper` trait represents each credential source, and a
`HelperChain` tries them in order: in-process cache first, then
`git credential`, with `GIT_ASKPASS` / `SSH_ASKPASS` and `~/.netrc`
slotting in as additional sources. Success and failure are broadcast
to every helper in the chain so caches stay in sync with the
upstream source of truth.

SSH remotes follow a different model. Rather than asking the user
for credentials, we run `git-lfs-authenticate <path> <operation>`
over SSH and parse a short-lived HTTPS token from the response. The
SSH key the user already manages is the only credential involved;
results are cached with the server-supplied expiry honored.
