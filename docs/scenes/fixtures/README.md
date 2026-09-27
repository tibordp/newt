# brewlog

A tiny command-line journal for coffee brews: log the dose, grind and time,
and see which recipes you keep coming back to.

## Install

```sh
cargo install brewlog
brewlog init ~/coffee
```

## Usage

| Command | What it does |
|---------|--------------|
| `brewlog add` | Record a brew interactively |
| `brewlog list --last 10` | Show recent brews |
| `brewlog best` | Rank recipes by your ratings |

> **Tip:** `brewlog add --from 12` copies brew #12 as a starting point,
> so repeating a recipe takes one keystroke.

## Roadmap

- [x] Ratings and notes
- [x] Export to CSV
- [ ] Grinder presets
- [ ] Water chemistry

See [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.
