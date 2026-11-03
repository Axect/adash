# Themes

adash ships 42 themes: its own `light`, `dark`, `high-contrast` and `ink`, and 38 built from published color schemes (Dracula, Catppuccin, Tokyo Night, Nord, Gruvbox, Solarized, Rosé Pine, Kanagawa, Everforest, GitHub, Modus, One, Ayu, Flexoki, the Nightfox family and an Okabe–Ito pair whose state colors stay apart under simulated color blindness). Until a device picks its own, it uses `dark` (adash dark) in dark mode and `catppuccin-latte` in light mode (`[theme]` in `config.toml` changes that). You can add your own as TOML files in the `themes` folder of the config directory (Linux `~/.config/adash/themes/`, macOS `~/Library/Application Support/adash/themes/`), or import your terminal's or editor's colors. Pick themes per device in **Display settings → Theme**: one for light mode, one for dark mode, or follow the system.

```sh
adash theme new solar --from dark     # writes a commented template
$EDITOR ~/.config/adash/themes/solar.toml
adash theme check ~/.config/adash/themes/solar.toml
adash theme list
adash theme export solar               # the generated CSS
adash theme show dracula               # a theme's TOML, with the palette color behind each value
adash theme import ghostty             # your terminal's colors as a theme (see Import)
```

The dashboard reloads a theme when you save the file; open browsers update without a reload.

## Format

A theme names a `base` and overrides only what it changes; every other token comes from the base (and the base's base, ending at `light`). The names `light`, `dark`, `high-contrast` and `ink` are reserved: a user file with one of them is refused and listed with the reason.

```toml
#:schema https://raw.githubusercontent.com/Axect/adash/main/docs/schema/theme.schema.json
name = "solar"
base = "dark"
density = "compact"

[brand]
accent = "#e8590c"

[ui]
bg = "#0b0f14"
surface = "#111827"
fg = "#e5e7eb"

[signal]
ask = "#c4b5fd"
error = "#fca5a5"
turn = "#f59e0b"

[font]
ui = "Inter, 'Noto Sans KR', system-ui, sans-serif"
size = 13
```

Values:

- colors: `"#rgb"`, `"#rrggbb"` or `"#rrggbbaa"`. Only `ui.selection`, `ui.border`, `ui.border-strong`, `signal.ask-bg`, `signal.error-bg`, `signal.turn-bg`, `timeline.idle` and the secondary text colors (`ui.muted`, `ui.dim`, `info.cache`, `info.progress`, `info.held`, `harness.*`) may be translucent; every other color must be opaque. Translucent text is checked after compositing over `ui.surface`.
- fonts: a comma-separated list of family names. A name is letters (any script), digits, spaces and `. _ -`, optionally wrapped in matching `'` or `"`. adash parses the list and writes it out again with every name quoted (generic families such as `sans-serif` or `ui-monospace` stay bare), so one value cannot reach into another declaration.
- `font.size`: 11 to 18 (px); the built-ins use 12.5.
- `density`: `"compact"` or `"comfortable"`.

A value that breaks these rules is an error. Unknown keys are reported as warnings and ignored. The generated stylesheet holds only the token declarations, so a theme can never add CSS rules. The `#:schema` line lets editors with [taplo](https://taplo.tamasfe.dev/) complete and validate the file ([docs/schema/theme.schema.json](schema/theme.schema.json)); the schema and the server share the input rules (which colors may be translucent, the font-list syntax and its 200-character limit). Contrast and inheritance are checked by the server only, when the theme loads or with `adash theme check`.

## Checks

A theme is resolved to the complete token set (its base first, then its own values) and then checked. Contrast is measured on the pairs the dashboard actually draws; translucent backgrounds are composited over `ui.surface` first.

| kind | pairs | error | warning |
|---|---|---|---|
| text | `ui.fg` on `ui.surface`, `ui.bg`, `ui.subtle`, `ui.selection`; `ui.muted`, `ui.link`, `signal.decision`, `info.cache`, `info.warn`, `info.thread`, `info.progress` on `ui.surface`; `signal.ask-text` on `signal.ask-bg` and `ui.surface`; `signal.error-text` on `signal.error-bg` and `ui.surface`; `signal.turn-text` on `signal.turn-bg` and on `ui.surface` (the TURN tag sits on the row); the wordmark's white text on `brand.pill` | under 3:1 | under 4.5:1 |
| marks | `signal.ask`, `signal.error`, `signal.turn`, `timeline.now`, `ui.interact` on `ui.surface` | under 2:1 | under 3:1 |
| button text | `ui.on-interact` on `ui.interact` (primary buttons, 11.5 px regular) | under 3:1 | under 4.5:1 |
| grayscale | `signal.ask`, `signal.error` against `timeline.working` (a working agent would look like one that needs you; the my-turn bar is a hollow rail, told apart by shape) | — | under 1.5:1 |

Also errors: syntax (reported with line and column), a bad value, transparency where it is not allowed, a missing base, a base that is the theme itself, a cycle of bases, or a chain deeper than 8 themes.

Inheritance errors are reported by every theme that inherits from the broken one, however deep. A theme with errors shows its base theme instead, after that base passed the same checks (or `light` when the base cannot be resolved), so a broken parent never leaks into its children. Display settings lists the errors and warnings; a theme with only warnings is used as written.

## Built-in themes

The built-in schemes use the published values only (exception: the Okabe–Ito pair takes its neutrals — backgrounds, text, borders and the light variant's interaction slate — from adash's own light and dark themes, since the Okabe–Ito palette has no neutrals); each theme file names the palette color behind every value (`adash theme show <name>`), and the palettes with their sources are in [`src/themes/`](../src/themes). One mapping turns a palette into a theme, for the built-ins and for imports alike:

- **Bars, chips and marks** use palette colors and need 3:1 on the card surface.
- **Text** needs 4.5:1. A role's own color is used when it reads; otherwise a shade of the same hue from the palette (20° or closer, and never the color or the hue of another state); otherwise the neutral foreground, while the role's own color stays on the chip's border, bar and icon. So Dracula's ERR label is its foreground on a red-bordered chip, not its orange.
- **Button text** (11.5 px, regular weight) needs 4.5:1 like any text. When the palette's blue or cyan cannot carry it, the primary button uses the foreground color as its fill (a neutral button).
- **Needs me**: error is the palette's red, question its purple (or magenta), my turn a calm hue — orange, then cyan, then green — never the brightest yellow of a dark palette and never the color of interaction, question or error. The dashboard draws the my-turn color in at most two places per row (the right-hand dot and the timeline's end rail); only question and error rows get a left edge bar.
- **Working** is a neutral (comment color) that stays 1.5:1 apart from the question and error bars in grayscale; **interaction** is the palette's blue or cyan.
- **Translucent** chip and selection backgrounds are palette colors at reduced alpha; nothing else is mixed.

| theme | source | error | question | my turn | interaction |
|---|---|---|---|---|---|
| Dracula | [draculatheme.com](https://draculatheme.com/spec) | Red | Purple | Orange | Cyan |
| Catppuccin Mocha | [github.com](https://github.com/catppuccin/palette/blob/main/palette.json) | Red | Mauve | Peach | Blue |
| Catppuccin Latte | [github.com](https://github.com/catppuccin/palette/blob/main/palette.json) | Red | Mauve | Teal | Text |
| Tokyo Night | [github.com](https://github.com/folke/tokyonight.nvim/blob/main/lua/tokyonight/colors/storm.lua) | red | purple | orange | blue |
| Tokyo Night Day | [github.com](https://github.com/folke/tokyonight.nvim/blob/main/extras/ghostty/tokyonight_day) | palette 1 | palette 5 | palette 6 | foreground |
| Nord | [www.nordtheme.com](https://www.nordtheme.com/docs/colors-and-palettes) | nord11 | nord15 | nord7 | nord9 |
| Gruvbox Dark | [github.com](https://github.com/morhetz/gruvbox/blob/master/colors/gruvbox.vim) | bright_red | bright_purple | bright_orange | bright_blue |
| Gruvbox Light | [github.com](https://github.com/morhetz/gruvbox/blob/master/colors/gruvbox.vim) | faded_red | faded_purple | faded_orange | faded_blue |
| Solarized Dark | [ethanschoonover.com](https://ethanschoonover.com/solarized/) | red | violet | orange | cyan |
| Solarized Light | [ethanschoonover.com](https://ethanschoonover.com/solarized/) | red | violet | orange | base03 |
| Rosé Pine | [github.com](https://github.com/rose-pine/neovim/blob/ff483051a47e27d84bdef47703538df1ed9f4a47/lua/rose-pine/palette.lua) | love | iris | pine | foam |
| Rosé Pine Moon | [github.com](https://github.com/rose-pine/neovim/blob/ff483051a47e27d84bdef47703538df1ed9f4a47/lua/rose-pine/palette.lua) | love | iris | pine | foam |
| Rosé Pine Dawn | [github.com](https://github.com/rose-pine/neovim/blob/ff483051a47e27d84bdef47703538df1ed9f4a47/lua/rose-pine/palette.lua) | love | iris | foam | pine |
| Kanagawa Wave | [github.com](https://github.com/rebelot/kanagawa.nvim/blob/bb85e4bfc8d89b0e62c8fa53ccdd13d12e2f77b3/lua/kanagawa/colors.lua) | autumnRed | oniViolet | surimiOrange | crystalBlue |
| Kanagawa Lotus | [github.com](https://github.com/rebelot/kanagawa.nvim/blob/bb85e4bfc8d89b0e62c8fa53ccdd13d12e2f77b3/lua/kanagawa/colors.lua) | lotusRed | lotusViolet4 | lotusYellow2 | lotusBlue4 |
| Everforest Dark (medium) | [github.com](https://github.com/sainnhe/everforest/blob/85a86eb62409e3ec88713bff3d1b9d7374e112e4/autoload/everforest.vim) | red | purple | orange | blue |
| Everforest Light (medium) | [github.com](https://github.com/sainnhe/everforest/blob/85a86eb62409e3ec88713bff3d1b9d7374e112e4/autoload/everforest.vim) | red | blue | grey2 | fg |
| GitHub Dark | [github.com](https://github.com/projekt0n/github-nvim-theme/blob/c106c9472154d6b2c74b74565616b877ae8ed31d/lua/github-theme/palette/primitives/dark.lua) | danger.fg | done.fg | severe.fg | accent.fg |
| GitHub Dark Dimmed | [github.com](https://github.com/projekt0n/github-nvim-theme/blob/c106c9472154d6b2c74b74565616b877ae8ed31d/lua/github-theme/palette/primitives/dark_dimmed.lua) | danger.fg | done.fg | severe.fg | accent.fg |
| GitHub Light | [github.com](https://github.com/projekt0n/github-nvim-theme/blob/c106c9472154d6b2c74b74565616b877ae8ed31d/lua/github-theme/palette/primitives/light.lua) | danger.fg | done.fg | severe.fg | accent.fg |
| Modus Vivendi | [github.com](https://github.com/protesilaos/modus-themes/blob/782743415b3eedac0b99db2342fb35703deb752f/modus-themes.el#L2056) | red | magenta-cooler | cyan | blue |
| Modus Operandi | [github.com](https://github.com/protesilaos/modus-themes/blob/782743415b3eedac0b99db2342fb35703deb752f/modus-themes.el#L749) | red | magenta-cooler | cyan | blue |
| One Dark | [github.com](https://github.com/atom/atom/blob/master/packages/one-dark-syntax/styles/colors.less) | hue-5 (red 1) | hue-3 (purple) | hue-6 (orange 1) | hue-2 (blue) |
| One Light | [github.com](https://github.com/atom/atom/blob/2c902d501e/packages/one-light-syntax/styles/colors.less) | hue-5 (red 1) | hue-3 (purple) | hue-6 (orange 1) | mono-1 (syntax-fg) |
| Ayu Dark | [github.com](https://github.com/ayu-theme/ayu-colors/blob/master/themes/dark.yaml) | syntax.markup | syntax.constant | syntax.keyword | syntax.entity |
| Ayu Mirage | [github.com](https://github.com/ayu-theme/ayu-colors/blob/master/themes/mirage.yaml) | syntax.markup | syntax.constant | syntax.keyword | syntax.entity |
| Ayu Light | [github.com](https://github.com/ayu-theme/ayu-colors/blob/master/themes/light.yaml) | common.error | syntax.constant | palette.orange.l1 | editor.fg |
| Flexoki Dark | [stephango.com](https://stephango.com/flexoki) | re (red-400) | pu (purple-400) | or (orange-400) | bl (blue-400) |
| Flexoki Light | [stephango.com](https://stephango.com/flexoki) | re (red-600) | pu (purple-600) | or (orange-600) | bl (blue-600) |
| Nightfox | [github.com](https://github.com/EdenEast/nightfox.nvim/blob/4dacd3f0185a2227bdf3b6c0975a8f0bf87cac9a/lua/nightfox/palette/nightfox.lua) | red.base | magenta.base | orange.base | blue.base |
| Dayfox | [github.com](https://github.com/EdenEast/nightfox.nvim/blob/4dacd3f0185a2227bdf3b6c0975a8f0bf87cac9a/lua/nightfox/palette/dayfox.lua) | red.base | magenta.base | orange.base | blue.base |
| Dawnfox | [github.com](https://github.com/EdenEast/nightfox.nvim/blob/4dacd3f0185a2227bdf3b6c0975a8f0bf87cac9a/lua/nightfox/palette/dawnfox.lua) | red.base | magenta.base | orange.dim | blue.base |
| Duskfox | [github.com](https://github.com/EdenEast/nightfox.nvim/blob/4dacd3f0185a2227bdf3b6c0975a8f0bf87cac9a/lua/nightfox/palette/duskfox.lua) | red.base | magenta.base | orange.base | blue.base |
| Nordfox | [github.com](https://github.com/EdenEast/nightfox.nvim/blob/4dacd3f0185a2227bdf3b6c0975a8f0bf87cac9a/lua/nightfox/palette/nordfox.lua) | red.base | magenta.base | orange.base | blue.base |
| Terafox | [github.com](https://github.com/EdenEast/nightfox.nvim/blob/4dacd3f0185a2227bdf3b6c0975a8f0bf87cac9a/lua/nightfox/palette/terafox.lua) | red.base | magenta.base | orange.base | blue.base |
| Carbonfox | [github.com](https://github.com/EdenEast/nightfox.nvim/blob/4dacd3f0185a2227bdf3b6c0975a8f0bf87cac9a/lua/nightfox/palette/carbonfox.lua) | red.base | magenta.base | cyan.base | blue.base |
| Okabe–Ito Dark | [jfly.uni-koeln.de](https://jfly.uni-koeln.de/color/) | vermillion | blue | sky blue | bluish green |
| Okabe–Ito Light | [jfly.uni-koeln.de](https://jfly.uni-koeln.de/color/) | vermillion | blue | bluish green | adash light muted |

Notes:
- Everforest Light's accents are under 3:1 on its background, so question uses its blue, interaction its `statusline2` slate and my turn `grey2`.
- Tokyo Night Day's foreground is 4.52:1 on its background, so no tint fits under its text: its chips have a clear background (the dashboard draws them with an outline).
- Okabe–Ito uses the Okabe & Ito color-universal-design palette with adash's neutrals. Its three state marks — error (vermillion), question (blue) and my turn (sky blue in dark, bluish green in light) — are at least ΔE*ab76 20 apart under simulated protanopia and deuteranopia (Machado et al. 2009, severity 1.0); a test checks this. That is a check of the chosen mark colors only: it does not cover small text, areas or backgrounds, and it does not guarantee that every user tells every element apart.
- Tokyo Night's values come from [storm.lua](https://github.com/folke/tokyonight.nvim/blob/main/lua/tokyonight/colors/storm.lua) with the background overrides of [night.lua](https://github.com/folke/tokyonight.nvim/blob/main/lua/tokyonight/colors/night.lua); its selection color `#283457` is the value upstream computes by blending (published in its [terminal extras](https://github.com/folke/tokyonight.nvim/blob/main/extras/ghostty/tokyonight_night)).

## Import

`adash theme import <source> [file|name]` reads a color scheme, maps it with the same rules and writes `<config dir>/themes/<source>-<scheme>.toml`, with a comment naming the source color of every value. It reports where the mapping had to move away from the obvious color (for example my turn on cyan because the scheme has no orange). Terminal colors are slotted by their actual hue, since terminals disagree on what "blue" and "magenta" hold.

```sh
adash theme import ghostty --dry-run        # print the theme, write nothing
adash theme import kitty --name my-kitty
adash theme import base16 ~/schemes/ocean.yaml --set-default   # also sets [theme] in config.toml
adash theme import ghostty --light          # the light half of a light:…,dark:… pair
```

| source | auto-detected | otherwise pass |
|---|---|---|
| `ghostty` | `~/.config/ghostty/config` (macOS: `~/Library/Application Support/com.mitchellh.ghostty/config`), `config-file` includes (applied after the whole including file, as Ghostty does; quoted values and `?optional` paths), `theme = name` or `light:…,dark:…` (looked up in `themes/` next to the config, then Ghostty's bundled themes), then `background`, `foreground`, `palette`, `selection-background`, `cursor-color` | a config or theme file |
| `base16`, `base24` | a scheme name in `$BASE16_SCHEMES` or `~/.local/share/tinted-theming/schemes` | a scheme YAML (classic or tinted-theming format); roles follow the base16 styling guide |
| `vscode` | `workbench.colorTheme` from the settings of VS Code, VSCodium, Cursor or Code - OSS, found in installed or built-in extensions, `include` chains followed; `terminal.ansi*` colors, else the syntax token colors (reported) | a theme JSON, or a theme label |
| `kitty` | `kitty.conf` (`$KITTY_CONFIG_DIRECTORY`, `~/.config/kitty`), `include` (inline) and `globinclude` with `*` and `?` in any path component (so `current-theme.conf` from `kitten themes`); other glob syntax (`**`, `[…]`, `{…}`) is refused | a conf file |
| `alacritty` | `alacritty.toml` with `import` / `general.import`; a legacy YAML config is refused with a hint to run `alacritty migrate` | a TOML file |
| `wezterm` | only a literal `color_scheme = "Name"` line in `wezterm.lua` (the Lua is not run), resolved in `~/.config/wezterm/colors/*.toml`; WezTerm's bundled schemes live inside its binary and are not readable | a scheme TOML file, or a scheme name present in `colors/` |
| `iterm2` | nothing (iTerm2 keeps profiles in a binary preferences plist) | an `.itermcolors` file (Settings → Profiles → Colors → Color Presets → Export) |

Every source is read-only. `--name N` and `--name=N` both set the theme name. Names and other text from the scheme are written through the TOML serializer. A theme whose output does not parse, or that has checker errors, is not written and not made the default, and the command exits with an error; warnings are printed and the theme is written.

## Color roles

Color means something on the board, so keep the roles apart:

- **brand**: the wordmark and favicon only;
- **interaction**: buttons, focus, selection, links, your own messages;
- **needs me**: question (`signal.ask`), error (`signal.error`), my turn / unread (`signal.turn`) — the only saturated colors on the board;
- **informational**: cache countdown, warnings, threads, progress;
- **neutral**: surfaces, text, borders, the working bar and harness glyphs.

## Tokens

| token | CSS variable | kind | used for |
|---|---|---|---|
| `brand.accent` | `--brand-accent` | color | wordmark speed lines and favicon only |
| `brand.pill` | `--brand-pill` | color | dark pill behind the wordmark |
| `ui.bg` | `--ui-bg` | color | page and project group header band |
| `ui.surface` | `--ui-surface` | color | agent rows, cards, dialogs |
| `ui.subtle` | `--ui-subtle` | color | column headers, footers |
| `ui.fg` | `--ui-fg` | color | primary text |
| `ui.muted` | `--ui-muted` | color | secondary text |
| `ui.dim` | `--ui-dim` | color | tertiary text |
| `ui.border` | `--ui-border` | color | 1 px separators |
| `ui.border-strong` | `--ui-border-strong` | color | group borders |
| `ui.interact` | `--ui-interact` | color | buttons, focus, selection, links, my message marks |
| `ui.on-interact` | `--ui-on-interact` | color | text on primary buttons (drawn on ui.interact) |
| `ui.link` | `--ui-link` | color | link and inline action text |
| `ui.selection` | `--ui-selection` | color | selected row background |
| `signal.ask` | `--signal-ask` | color | question waiting (needs me, 1st) |
| `signal.ask-bg` | `--signal-ask-bg` | color | question chip background |
| `signal.ask-text` | `--signal-ask-text` | color | question label and chip text |
| `signal.error` | `--signal-error` | color | stopped on an error (needs me, 2nd) |
| `signal.error-bg` | `--signal-error-bg` | color | error chip background |
| `signal.error-text` | `--signal-error-text` | color | error label and chip text |
| `signal.turn` | `--signal-turn` | color | my turn / unread answer (needs me, 3rd) |
| `signal.turn-text` | `--signal-turn-text` | color | text on turn chips |
| `signal.turn-bg` | `--signal-turn-bg` | color | turn chip background |
| `signal.decision` | `--signal-decision` | color | decisions for me |
| `info.cache` | `--info-cache` | color | cache countdown (informational) |
| `info.warn` | `--info-warn` | color | capacity and held warnings |
| `info.thread` | `--info-thread` | color | agent-to-agent threads |
| `info.progress` | `--info-progress` | color | progress counters |
| `info.held` | `--info-held` | color | held deliveries |
| `timeline.working` | `--timeline-working` | color | working bar (drawn opaque) |
| `timeline.idle` | `--timeline-idle` | color | idle track |
| `timeline.snoozed` | `--timeline-snoozed` | color | snoozed hatch |
| `timeline.now` | `--timeline-now` | color | now line |
| `harness.omp` | `--harness-omp` | color | omp glyph tint |
| `harness.cc` | `--harness-cc` | color | Claude Code glyph tint |
| `harness.cdx` | `--harness-cdx` | color | Codex glyph tint |
| `harness.agy` | `--harness-agy` | color | Antigravity glyph tint |
| `font.ui` | `--font-ui` | font | UI font family list |
| `font.mono` | `--font-mono` | font | monospace font family list |
| `font.size` | `--font-size` | size | base font size in px (11 to 18) |
| `density` | `--density` | density | compact | comfortable |

## Example: a warm light theme

```toml
name = "paper"
base = "light"

[ui]
bg = "#f4f1ea"
surface = "#fbfaf7"
subtle = "#f1ede4"
border = "#e3ddd0"
fg = "#1f1b16"

[brand]
accent = "#c2410c"
```
