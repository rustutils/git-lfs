- `lfs.url` / `remote.<remote>.lfsurl`

  The URL of the Git LFS API endpoint. Defaults to deriving the endpoint from the clone URL (`<clone-url>/info/lfs`). The remote-scoped form overrides the general one for a particular remote.

- `lfs.pushurl` / `remote.<remote>.lfspushurl`

  Same idea but consulted only when pushing. Defaults to `lfs.url` or the derived endpoint.

- `lfs.<url>.access`

  Authentication mode for the LFS endpoint at `<url>`. Either `basic` (HTTP basic auth via the credential helper, the default after a successful round-trip) or `none` (no authentication). Set via `git config --add` when the access mode for an endpoint should be persisted; the auth-retry loop also writes this on a successful 401-fill cycle.

- `core.askpass` / `GIT_ASKPASS`

  Program invoked when interactive credentials are needed against the LFS API. Stdout is read as the credential value. Same selection priority as Git uses: `GIT_ASKPASS` env beats `core.askpass`, which beats `SSH_ASKPASS`.

- `credential.helper`, `credential.useHttpPath`, `credential.protectProtocol`

  Standard Git credential-helper plumbing. `useHttpPath=true` distinguishes credentials per path within a host (so two paths on the same domain can have different passwords). `protectProtocol=false` lets credentials with carriage returns through (default `true`).
