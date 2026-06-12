# Bundled syntect themes

Canonical `.tmTheme` ports embedded into the binary (see
`markdown::highlight::bundled_theme`) to pair fenced-code highlighting with
the named editor palettes. Only the selected theme is parsed at startup.

| Files | Source | License |
| --- | --- | --- |
| `catppuccin-mocha`, `catppuccin-latte` | [catppuccin/bat](https://github.com/catppuccin/bat) | MIT |
| `tokyonight-night`, `tokyonight-day` | [folke/tokyonight.nvim](https://github.com/folke/tokyonight.nvim) (extras/sublime) | Apache-2.0 |
| `onehalf-dark`, `onehalf-light` | [sonph/onehalf](https://github.com/sonph/onehalf) | MIT |
| `gruvbox-dark`, `gruvbox-light` | [subnut/gruvbox-tmTheme](https://github.com/subnut/gruvbox-tmTheme) (bat's gruvbox source) | MIT |
| `dracula` | [dracula/sublime](https://github.com/dracula/sublime) | MIT |
| `nord` | [crabique/Nord-plist](https://github.com/crabique/Nord-plist) | MIT |

Solarized and InspiredGitHub pairings use syntect's built-in defaults and are
not bundled here.
