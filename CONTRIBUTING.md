# Contributing to 6Stars

Small fixes are welcome, even if this is your first PR. You can also help by reproducing bugs, testing on macOS or Windows, improving the docs, or writing down what you learned about a file format.

## Issues and ideas

[Open an issue](https://github.com/Ahmet-Dedeler/6Stars/issues/new) if something breaks or you have an idea. For bugs, tell us your 6Stars version, OS, what you tried, and what happened. Screenshots, logs, and small synthetic examples help. Please don't attach copyrighted game files, keys, or private data.

If you're planning a bigger change or aren't sure where to start, open an issue first so we can figure it out together. No need to ask before fixing a typo.

## Commits and pull requests

1. Make a branch from the 6Stars `main` branch and open your PR against `Ahmet-Dedeler/6Stars:main`. You don't need write access to send a PR.
2. Keep the change focused. Any clear commit message is fine, like `Fix audio seek after pausing`. You don't need a signed-off commit or a separate contributor agreement.
3. Tell us what changed, why, and how you checked it. Add a test if it would catch the bug again. For Rust changes, run `cargo test --release` if you can. If you only have one OS, say what you tested; Windows CI runs on PRs. For docs, just check the text and links.

PRs are reviewed before they land. If you'd like to help regularly, say hi in an issue. We'd love help reviewing PRs, sorting bugs, and testing releases, and we can talk about maintainer access once we've worked together.

## What you can share

PRs are welcome for work you have permission to share, including new docs and changes to the [MIT-licensed modules](README.md#license). You keep your copyright. Submit changes to MIT-licensed files under MIT, and flag any third-party material before adding it.

The [PercyStar license](LICENSE) still applies to inherited code. If you can share your change but can't open a PR without redistributing inherited code, post just your change in an issue. For fixes that need the copyright holder's written permission, describe the fix in an issue and wait for permission before posting a patch. 6Stars can't grant that permission.

Please follow the [code of conduct](CODE_OF_CONDUCT.md) in issues, reviews, and PRs.
