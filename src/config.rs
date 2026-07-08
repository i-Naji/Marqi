//! User configuration, loaded from `config.toml` in the platform config dir
//! (`~/.config/marqi/config.toml` on Linux, `~/Library/Application Support` on
//! macOS). Everything is optional; missing values fall back to defaults.
//!
//! ```toml
//! [editor]
//! keybindings = "standard"   # "standard" | "vim" | "nano" | "emacs"
//! line_numbers = "off"       # "off" | "absolute" | "relative"
//! heading_glyphs = true
//! left_margin = 1
//! tab_width = 4
//! scrolloff = 3
//!
//! [theme]
//! name = "default"                 # palette family: "default" picks One Dark
//!                                  # (dark) / GitHub (light); also: "marqi",
//!                                  # "onedark", "github", "catppuccin",
//!                                  # "tokyonight", "gruvbox", "nord",
//!                                  # "dracula", "solarized"
//! variant = "auto"                 # "auto" | "dark" | "light"
//! # syntax = "base16-ocean.dark"   # optional syntect theme override
//!
//! [theme.markdown]                # element -> #rrggbb overrides
//! heading1 = "#8be9fd"
//! link = "#8be9fd"
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::color::parse_hex;
use crate::markdown::{CodeHighlighter, MarkdownTheme};

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub editor: EditorConfig,
    pub theme: ThemeConfig,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditorConfig {
    pub keybindings: String,
    pub line_numbers: String,
    pub heading_glyphs: bool,
    /// How a single source newline renders in the preview: "space" (markdown
    /// reflow) or "break" (a visible line break, Obsidian-style).
    pub soft_break: String,
    /// Blank columns between the terminal's left edge and the editor content
    /// (0 disables).
    pub left_margin: usize,
    pub tab_width: usize,
    pub scrolloff: usize,
    /// Save automatically ~2 seconds after the last edit (named buffers only).
    pub auto_save: bool,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            keybindings: "standard".to_string(),
            line_numbers: "off".to_string(),
            heading_glyphs: true,
            soft_break: "space".to_string(),
            left_margin: 1,
            tab_width: 4,
            scrolloff: 3,
            auto_save: false,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    /// Palette family name ("onedark", "github", "catppuccin", ...). Empty or
    /// "default" picks the most popular face per variant: One Dark when dark,
    /// GitHub when light. "marqi" keeps the original palette.
    pub name: String,
    /// Built-in markdown/editor palette variant: "auto", "dark", or "light".
    pub variant: String,
    /// A syntect theme name for fenced code blocks.
    pub syntax: Option<String>,
    /// Per-element colour overrides (`element -> "#rrggbb"`).
    pub markdown: HashMap<String, String>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            variant: "auto".to_string(),
            syntax: None,
            markdown: HashMap::new(),
        }
    }
}

impl Config {
    /// Load the config file and return a short warning for readable-but-invalid
    /// or unreadable config files. A missing config is not an error.
    pub fn load_with_warning() -> (Self, Option<String>) {
        let Some(path) = Self::path() else {
            return (Self::default(), None);
        };
        Self::load_from_path(&path)
    }

    /// Load config from an explicit `-c` argument: either a TOML file or a
    /// directory containing `config.toml`. Unlike the implicit platform path,
    /// a missing file here warns: the user asked for this path, so silently
    /// using defaults would hide a typo'd `-c`.
    pub fn load_from_arg_with_warning(arg: impl AsRef<Path>) -> (Self, Option<String>) {
        let arg = arg.as_ref();
        let path = if arg.is_dir() {
            arg.join("config.toml")
        } else {
            arg.to_path_buf()
        };
        if !path.exists() {
            return (
                Self::default(),
                Some(format!("Config not found: {}", path.display())),
            );
        }
        Self::load_from_path(&path)
    }

    /// The expected config file path, if a config directory can be resolved.
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "marqi")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    fn load_from_path(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("Config ignored: {} ({e})", path.display())),
                );
            }
        };
        match toml::from_str::<Self>(&text) {
            Ok(config) => {
                let warning = config.validate_warning();
                (config, warning)
            }
            Err(e) => (
                Self::default(),
                Some(format!("Config ignored: {} ({e})", path.display())),
            ),
        }
    }

    /// Warn about values that parse as strings but match no known variant —
    /// they would otherwise silently fall back to the default downstream.
    fn validate_warning(&self) -> Option<String> {
        let fields = [
            (
                "editor.keybindings",
                &self.editor.keybindings,
                &["standard", "vim", "nano", "emacs"][..],
            ),
            (
                "editor.line_numbers",
                &self.editor.line_numbers,
                &["off", "absolute", "relative"],
            ),
            (
                "editor.soft_break",
                &self.editor.soft_break,
                &["space", "break"],
            ),
            (
                "theme.variant",
                &self.theme.variant,
                &["auto", "dark", "light"],
            ),
        ];
        let mut bad: Vec<String> = fields
            .into_iter()
            .filter(|(_, value, allowed)| {
                let v = value.trim().to_ascii_lowercase();
                !allowed.contains(&v.as_str())
            })
            .map(|(name, value, _)| format!("{name} = \"{value}\""))
            .collect();
        if !MarkdownTheme::is_known_name(&self.theme.name) {
            bad.push(format!("theme.name = \"{}\"", self.theme.name));
        }
        if let Some(syntax) = self.theme.syntax.as_deref()
            && !CodeHighlighter::has_theme(syntax)
        {
            bad.push(format!("theme.syntax = \"{syntax}\""));
        }
        if self.editor.tab_width == 0 {
            bad.push("editor.tab_width = 0".to_string());
        }

        let mut overrides: Vec<_> = self.theme.markdown.iter().collect();
        overrides.sort_by_key(|(key, _)| *key);
        for (key, value) in overrides {
            if !MarkdownTheme::is_override_key(key) {
                bad.push(format!("theme.markdown.{key}"));
            } else if parse_hex(value).is_none() {
                bad.push(format!("theme.markdown.{key} = \"{value}\""));
            }
        }
        (!bad.is_empty()).then(|| format!("Config: invalid setting(s): {}", bad.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("marqi_config_{tag}_{}.toml", std::process::id()));
        p
    }

    #[test]
    fn theme_name_parses_from_toml() {
        let path = temp_path("theme_name");
        std::fs::write(
            &path,
            "[theme]\nname = \"catppuccin\"\nvariant = \"dark\"\n",
        )
        .unwrap();
        let (cfg, warning) = Config::load_from_path(&path);
        std::fs::remove_file(&path).ok();
        assert!(warning.is_none(), "warning: {warning:?}");
        assert_eq!(cfg.theme.name, "catppuccin");
        assert_eq!(cfg.theme.variant, "dark");
    }

    #[test]
    fn missing_config_is_quiet_default() {
        let path = temp_path("missing");
        std::fs::remove_file(&path).ok();
        let (cfg, warning) = Config::load_from_path(&path);
        assert!(warning.is_none());
        assert_eq!(cfg.editor.keybindings, "standard");
        assert_eq!(cfg.editor.line_numbers, "off");
        assert!(cfg.editor.heading_glyphs);
        assert_eq!(cfg.editor.left_margin, 1);
        assert_eq!(cfg.theme.variant, "auto");
        assert_eq!(
            cfg.theme.name, "",
            "unset name selects the per-mode default"
        );
    }

    #[test]
    fn left_margin_accepts_zero_and_rejects_negatives() {
        let cfg: Config = toml::from_str("[editor]\nleft_margin = 0\n").unwrap();
        assert_eq!(cfg.editor.left_margin, 0);

        let path = temp_path("negative_margin");
        std::fs::write(&path, "[editor]\nleft_margin = -1\n").unwrap();
        let (cfg, warning) = Config::load_from_path(&path);
        assert!(warning.is_some(), "a negative margin must warn");
        assert_eq!(cfg.editor.left_margin, 1, "and fall back to the default");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn invalid_config_returns_warning_and_defaults() {
        let path = temp_path("invalid");
        std::fs::write(&path, "[editor\n").unwrap();
        let (cfg, warning) = Config::load_from_path(&path);
        assert!(warning.is_some());
        assert_eq!(cfg.editor.tab_width, 4);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unknown_keys_and_values_warn() {
        let path = temp_path("typo_key");
        std::fs::write(&path, "[editor]\nline_numers = \"relative\"\n").unwrap();
        let (_, warning) = Config::load_from_path(&path);
        assert!(warning.is_some(), "a typo'd key must not pass silently");
        std::fs::remove_file(&path).ok();

        let path = temp_path("typo_value");
        std::fs::write(&path, "[editor]\nkeybindings = \"vmi\"\n").unwrap();
        let (cfg, warning) = Config::load_from_path(&path);
        assert!(warning.is_some(), "an unknown value must warn");
        assert_eq!(cfg.editor.keybindings, "vmi", "the config still loads");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn invalid_theme_settings_warn() {
        let path = temp_path("invalid_theme");
        std::fs::write(
            &path,
            "[editor]\ntab_width = 0\n[theme]\nname = \"mystery\"\nsyntax = \"missing\"\n[theme.markdown]\nheading1 = \"red\"\nheding2 = \"#ffffff\"\n",
        )
        .unwrap();

        let (_, warning) = Config::load_from_path(&path);
        let warning = warning.expect("invalid theme settings should warn");
        assert!(warning.contains("editor.tab_width = 0"));
        assert!(warning.contains("theme.name"));
        assert!(warning.contains("theme.syntax"));
        assert!(warning.contains("theme.markdown.heading1"));
        assert!(warning.contains("theme.markdown.heding2"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn explicit_dir_without_config_warns() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("marqi_no_config_dir_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::remove_file(dir.join("config.toml")).ok();
        let (_, warning) = Config::load_from_arg_with_warning(&dir);
        assert!(warning.is_some(), "-c with no config.toml must warn");
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn explicit_config_dir_loads_config_toml() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("marqi_config_dir_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[editor]\nkeybindings = \"emacs\"\n",
        )
        .unwrap();

        let (cfg, warning) = Config::load_from_arg_with_warning(&dir);

        assert!(warning.is_none());
        assert_eq!(cfg.editor.keybindings, "emacs");
        std::fs::remove_file(dir.join("config.toml")).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn explicit_config_file_loads_directly() {
        let path = temp_path("direct_file");
        std::fs::write(&path, "[editor]\nline_numbers = \"relative\"\n").unwrap();

        let (cfg, warning) = Config::load_from_arg_with_warning(&path);

        assert!(warning.is_none());
        assert_eq!(cfg.editor.line_numbers, "relative");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn explicit_missing_path_warns() {
        let path = temp_path("no_such");
        std::fs::remove_file(&path).ok();
        let (_, warning) = Config::load_from_arg_with_warning(&path);
        assert!(warning.is_some(), "-c with a missing path must warn");
    }
}
