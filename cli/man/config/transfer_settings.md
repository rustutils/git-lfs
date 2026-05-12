- `lfs.concurrenttransfers`

  Number of object transfers running in parallel within a single LFS command. Default 8.

- `lfs.basictransfersonly`

  When `true`, restrict the client to the basic HTTP upload/download adapter, ignoring more advanced transfers the server may advertise. Useful for working around broken intermediaries. Default `false`.

- `lfs.transfer.batchSize`

  Max objects per `POST /objects/batch` request. The transfer queue chunks the input list into runs of this size and issues one batch call per chunk. Default 100. Values < 1 are clamped to 1. Servers may refuse oversize batches with 413; lower this if you see those.

- `lfs.transfer.enablehrefrewrite`

  When `true`, applies `url.<base>.insteadOf` / `url.<base>.pushInsteadOf` rewrites to the action URLs the batch endpoint hands back. `pushInsteadOf` is used for upload actions; `insteadOf` is used for downloads and for uploads when `pushInsteadOf` isn't set. Default `false`.

- `lfs.<url>.contenttype`

  When `true` (the default), the basic upload adapter sniffs the first 512 bytes of each object and sets the `Content-Type` header on the PUT to the detected MIME type. Set to `false` to send `application/octet-stream` unconditionally — useful when a CDN rejects uploads based on content sniffing. The batch response's `action.header` always wins if it pins a Content-Type itself.

- `lfs.<url>.sshtransfer`

  Whether to use SSH (`git-lfs-authenticate`) for the LFS endpoint at `<url>`. Values: `negotiate` (try SSH first, fall back to HTTPS — the default for `ssh://` and `git@` remotes), `always`, or `never`.
