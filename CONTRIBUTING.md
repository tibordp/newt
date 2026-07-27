# Contributing to Newt

## Issues first

Issues, bug reports, and feature requests are the preferred form of contribution - they are
valuable on their own and never go to waste. Code contributions are welcome too, but with a
caveat: there is no guarantee a pull request will be merged, and merged code may be substantially
rewritten to fit the architecture and style of the project. Contribution credit and attribution
will be preserved to the extent reasonable, even when the code itself changes shape.

If you are considering a larger change, opening an issue to discuss it first will save everyone
time.

## AI contributions

AI-assisted and AI-generated contributions are welcome and encouraged - much of this codebase is
built that way. You are encouraged to share the prompts or the workflow you used alongside the
change or issue itself; it is often as interesting as the change, and it helps with review.

## Testing

Unless a change is trivial or non-functional, exercise it manually on at least one supported
platform, and preferably on more than one. A platform-specific change has to be exercised on the
platform it targets. This is in addition to the automated tests, not instead of them - say in the
pull request what you ran it on.

## Architecture and style

`CLAUDE.md` covers the architecture, its invariants, and the footguns worth knowing about, and the
module headers cover their own subsystems; read the relevant parts before changing them. Build and
test commands are in the [README](README.md).

## Licensing

Newt is [GPL-3.0-or-later](LICENSE), and contributions are licensed under the same terms. There is
no CLA and no copyright assignment - you retain the copyright in what you write. New dependencies
and bundled assets have to be GPL-compatible.
