# Contributing to 6Stars

Thanks for taking a look at 6Stars. You don't need to know Rust or understand the whole codebase to help. If a build step tripped you up, a preview looked wrong, or you worked out what a few bytes mean, we'd like to hear about it.

## Issues and ideas

[Open an issue](https://github.com/Ahmet-Dedeler/6Stars/issues/new) with what you saw. Something like "on macOS, I opened an AWC bank in 6Stars 1.4.1 and the second stream wouldn't play" is enough to get started. Screenshots, logs, and little examples you made yourself can help. Leave out game files, keys, and anything private.

Have a bigger change in mind, or want an idea for a first PR? Tell us what you're interested in and we can find a place to start. For something small like a typo, you can go straight to the PR.

## Commits and pull requests

1. Start from 6Stars `main` and point your PR at `Ahmet-Dedeler/6Stars:main`. You don't need write access. If you're not sure what you can send, see [what you can share](#what-you-can-share).
2. A small PR is easier to review. Give the commit a name that says what it does, like `Fix audio seek after pausing`. No special format, sign-off, or separate contributor agreement.
3. A sentence or two on what you changed and how you checked it is fine. For Rust changes, try `cargo test --release` and add a test if it would catch the bug again. Only have one OS? Say which one you tested. Windows CI runs on PRs. For docs, reading the edit and checking the links is enough.

We'll review the PR before merging. If you'd like to stick around, say hi in an issue. You can help review changes, sort through bug reports, or try new releases. Once we've worked together a bit, we can talk about maintainer access.

## What you can share

New docs and changes to the [MIT-licensed modules](README.md#license) are good places to start. You keep your copyright, and changes to MIT files come in under MIT too. If you want to include something someone else wrote, let us know before adding it.

The [PercyStar license](LICENSE) still applies to code we inherited. If you have a change you're allowed to share but can't send a PR without redistributing that code, post just your change in an issue. If the patch itself needs the copyright holder's written permission, describe the fix in the issue for now. 6Stars can't give permission on their behalf.

See the [code of conduct](CODE_OF_CONDUCT.md) for how we work together in issues and reviews.
