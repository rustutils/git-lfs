Convert specific file types in unpushed commits to LFS:

    git lfs migrate import --include="*.mp3,*.psd"

Convert all zip files across every local branch:

    git lfs migrate import --everything --include="*.zip"

Convert every file over 100K in every local branch:

    git lfs migrate import --everything --above=100Kb

Repair already-committed files that *should* be LFS pointers according to `.gitattributes` but aren't (e.g. committed while `git lfs install` wasn't active):

    git lfs migrate import --fixup

Migrate to Git LFS in a single new commit on top of HEAD without rewriting history:

    git lfs track "*.zip" "*.mp3" "*.psd"
    git add .gitattributes
    git commit -m "add Git LFS attributes"
    git lfs migrate import --no-rewrite test.zip audios/*.mp3 images/*.psd

After any history-rewriting migration, force-push the rewritten branches — this alters Git history on your remotes and should be done with care.
