List the patterns that Git LFS is currently tracking:

    git lfs track

Configure Git LFS to track GIF files:

    git lfs track "*.gif"

Configure Git LFS to track PSD files and make them read-only unless
locked:

    git lfs track --lockable "*.psd"

Configure Git LFS to track the file named `project [1].psd`:

    git lfs track --filename "project [1].psd"
