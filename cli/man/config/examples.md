Configure a custom LFS endpoint for everyone who clones the repository:

    git config -f .lfsconfig lfs.url https://lfs.example.com/foo/bar/info/lfs

Set the endpoint locally for the current user without touching `.lfsconfig`:

    git config --global lfs.url https://lfs.example.com/foo/bar/info/lfs

Disable lock verification at the pre-push hook for a specific endpoint:

    git config --global lfs.https://lfs.example.com/.locksverify false

Raise the concurrent-transfer ceiling on a fast link:

    git config --global lfs.concurrenttransfers 16

Exclude a large media subtree from fetch/checkout:

    git config lfs.fetchexclude "media/raw/**,**/*.psd"
