List the file types taking up the most space in your repository's
unpushed commits:

    git lfs migrate info

Convert specific file types in unpushed commits to LFS:

    git lfs migrate import --include="*.mp3,*.psd"

Check for large files and existing LFS objects across every branch:

    git lfs migrate info --everything

Convert all zip files in every local branch to LFS:

    git lfs migrate import --everything --include="*.zip"

Convert all files over 100K in every local branch:

    git lfs migrate import --everything --above=100Kb

Migrate to Git LFS in a single new commit (no history rewrite):

    git lfs track "*.zip" "*.mp3" "*.psd"
    git add .gitattributes
    git commit -m "add Git LFS attributes"
    git lfs migrate import --no-rewrite --yes test.zip audios/*.mp3 images/*.psd

Convert all zip Git LFS objects back to regular Git blobs:

    git lfs migrate export --include-ref=main --include="*.zip"

After any history-rewriting migration, force-push the rewritten
branches to your remotes — this alters Git history on your remotes
and should be done with care.
