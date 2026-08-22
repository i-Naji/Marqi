use super::App;
use regex::Regex;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

static FOOTNOTE_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\^([^\]]+)\]").unwrap());
static INLINE_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[^\]]+\]\(([^)\s]+)").unwrap());
static ANGLE_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(https?://[^>]+)>").unwrap());
static BARE_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(https?://[^\s<>)]+)").unwrap());
static REF_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\[([^\]]*)\]").unwrap());
static REF_LINK_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\[[^\]]*\]").unwrap());
static FOOTNOTE_DEF_AT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?mi)^[ \t]*\[\^([^\]]+)\]:").unwrap());

impl App {
    pub(super) fn follow_link(&mut self) {
        let source = self.buffer.rope().to_string();
        let cursor = self.cursor.byte;

        if let Some(id) = footnote_definition_at(&source, cursor) {
            if let Some(target) = footnote_reference(&source, &id) {
                self.jump_to_byte(target);
            } else {
                self.status = Some(format!("No reference for footnote {id}"));
            }
            return;
        }
        if let Some(id) = capture_at(&source, cursor, &FOOTNOTE_REF, 1) {
            if let Some(target) = footnote_definition(&source, &id) {
                self.jump_to_byte(target);
            } else {
                self.status = Some(format!("Footnote {id} is unresolved"));
            }
            return;
        }

        let target = capture_at(&source, cursor, &INLINE_LINK, 1)
            .or_else(|| capture_at(&source, cursor, &ANGLE_URL, 1))
            .or_else(|| capture_at(&source, cursor, &BARE_URL, 1))
            .or_else(|| {
                capture_at(&source, cursor, &REF_LINK, 2).and_then(|id| {
                    let id = if id.is_empty() {
                        capture_at(&source, cursor, &REF_LINK_LABEL, 1)?
                    } else {
                        id
                    };
                    reference_target(&source, &id)
                })
            });

        match target {
            Some(target) if target.starts_with("http://") || target.starts_with("https://") => {
                self.open_external_url_prompt(target)
            }
            Some(target) => self.status = Some(format!("Local link target: {target}")),
            None => self.status = Some("No link or footnote under cursor".to_string()),
        }
    }

    fn jump_to_byte(&mut self, byte: usize) {
        self.cursor.byte = byte.min(self.buffer.len_bytes());
        self.selection_anchor = None;
        self.follow_cursor = true;
        self.ensure_layout();
        self.cursor.sync_goal(&self.layout);
    }
}

fn capture_at(source: &str, cursor: usize, regex: &Regex, group: usize) -> Option<String> {
    regex.captures_iter(source).find_map(|captures| {
        let whole = captures.get(0)?;
        if cursor < whole.start() || cursor > whole.end() {
            return None;
        }
        captures.get(group).map(|found| found.as_str().to_string())
    })
}

fn reference_target(source: &str, id: &str) -> Option<String> {
    let pattern = format!(r"(?mi)^[ \t]*\[{}\]:[ \t]*(\S+)", regex::escape(id));
    Regex::new(&pattern)
        .ok()?
        .captures(source)?
        .get(1)
        .map(|target| target.as_str().trim_matches(['<', '>']).to_string())
}

fn footnote_definition(source: &str, id: &str) -> Option<usize> {
    let pattern = format!(r"(?mi)^[ \t]*\[\^{}\]:", regex::escape(id));
    Regex::new(&pattern)
        .ok()?
        .find(source)
        .map(|found| found.start())
}

fn footnote_reference(source: &str, id: &str) -> Option<usize> {
    let pattern = format!(r"(?i)\[\^{}\]", regex::escape(id));
    Regex::new(&pattern)
        .ok()?
        .find_iter(source)
        .find_map(|found| {
            (source.as_bytes().get(found.end()) != Some(&b':')).then_some(found.start())
        })
}

fn footnote_definition_at(source: &str, cursor: usize) -> Option<String> {
    FOOTNOTE_DEF_AT.captures_iter(source).find_map(|captures| {
        let whole = captures.get(0)?;
        (cursor >= whole.start() && cursor <= whole.end())
            .then(|| captures.get(1).map(|id| id.as_str().to_string()))
            .flatten()
    })
}

pub(super) fn launch_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}
