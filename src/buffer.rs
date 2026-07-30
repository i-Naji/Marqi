//! The text buffer: a [`ropey::Rope`] holding the markdown source, plus the
//! associated file path and a modified flag.
//!
//! Besides reading and saving, this type exposes the byte-range `insert`,
//! `remove`, and `slice` primitives the editor's grapheme-aware cursor builds
//! on; the `modified` flag tracks unsaved changes.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use ropey::Rope;

const MAX_RECOVERY_BYTES: usize = 32 * 1024 * 1024;

/// An in-memory document backed by a rope.
pub struct TextBuffer {
    rope: Rope,
    path: Option<PathBuf>,
    /// Display name for buffers without a path (e.g. piped stdin).
    name: Option<String>,
    modified: bool,
    /// Whether new lines should be `\r\n` (the file's dominant ending), so
    /// editing a CRLF file does not produce mixed endings.
    crlf: bool,
    disk_state: DiskState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiskState {
    Missing,
    Present(DiskFingerprint),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DiskFingerprint {
    len: u64,
    modified: Option<SystemTime>,
    hash: u64,
}

impl TextBuffer {
    /// An empty, unnamed buffer.
    pub fn empty() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            name: None,
            modified: false,
            crlf: false,
            disk_state: DiskState::Missing,
        }
    }

    /// A pathless buffer holding `content` (e.g. markdown piped on stdin),
    /// shown under `name` in the status bar. Saving requires giving it a path.
    pub fn scratch(content: &str, name: &str) -> Self {
        Self {
            rope: Rope::from_str(content),
            path: None,
            name: Some(name.to_string()),
            modified: false,
            crlf: detect_crlf(content),
            disk_state: DiskState::Missing,
        }
    }

    /// Load a file into a rope. A path that does not exist yet yields an empty
    /// buffer remembering that path (so a later save creates the file).
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let (rope, crlf, disk_state) = if path.exists() {
            let text =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            (
                Rope::from_str(&text),
                detect_crlf(&text),
                fingerprint(path, text.as_bytes())?,
            )
        } else {
            (Rope::new(), false, DiskState::Missing)
        };
        Ok(Self {
            rope,
            path: Some(path.to_path_buf()),
            name: None,
            modified: false,
            crlf,
            disk_state,
        })
    }

    /// The line ending to insert for new lines, matching the file's dominant
    /// existing style.
    pub fn newline(&self) -> &'static str {
        if self.crlf { "\r\n" } else { "\n" }
    }

    /// Whether the buffer has a file path to save to.
    pub fn has_path(&self) -> bool {
        self.path.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn has_external_change(&self) -> Result<bool> {
        let Some(path) = self.path.as_deref() else {
            return Ok(false);
        };
        Ok(fingerprint_from_disk(path)? != self.disk_state)
    }

    /// Write the buffer back to its file atomically. Errors if the buffer is
    /// unnamed.
    pub fn save(&mut self) -> Result<()> {
        if self.has_external_change()? {
            anyhow::bail!("file changed on disk");
        }
        self.save_force()
    }

    pub fn save_force(&mut self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .context("no file name; nowhere to save")?;
        write_atomic(path, &self.rope).with_context(|| format!("saving {}", path.display()))?;
        self.disk_state = fingerprint_from_disk(path)?;
        self.modified = false;
        let _ = self.discard_recovery();
        Ok(())
    }

    pub fn reload(&mut self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .context("no file name; nowhere to reload")?;
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        self.rope = Rope::from_str(&text);
        self.crlf = detect_crlf(&text);
        self.disk_state = fingerprint(path, text.as_bytes())?;
        self.modified = false;
        Ok(())
    }

    /// Write the buffer to `path` atomically and adopt that path on success
    /// ("save as"). On error the buffer keeps its previous identity, so a
    /// mistyped path does not bind the buffer to an unwritable destination.
    pub fn save_as(&mut self, path: PathBuf) -> Result<()> {
        write_atomic(&path, &self.rope).with_context(|| format!("saving {}", path.display()))?;
        self.disk_state = fingerprint_from_disk(&path)?;
        self.path = Some(path);
        self.name = None;
        self.modified = false;
        let _ = self.discard_recovery();
        Ok(())
    }

    pub fn write_recovery(&self) -> Result<()> {
        if self.len_bytes() > MAX_RECOVERY_BYTES {
            anyhow::bail!("document is too large for automatic recovery");
        }
        let path = self
            .recovery_path()
            .context("recovery directory is unavailable")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        write_atomic(&path, &self.rope)
            .with_context(|| format!("writing recovery {}", path.display()))
    }

    pub fn load_recovery(&self) -> Result<Option<String>> {
        let Some(path) = self.recovery_path() else {
            return Ok(None);
        };
        let recovered = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        if recovered == self.rope {
            let _ = fs::remove_file(path);
            return Ok(None);
        }
        Ok(Some(recovered))
    }

    pub fn discard_recovery(&self) -> Result<()> {
        let Some(path) = self.recovery_path() else {
            return Ok(());
        };
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
        }
    }

    pub fn restore_recovery(&mut self, content: &str) {
        self.rope = Rope::from_str(content);
        self.crlf = detect_crlf(content);
        self.modified = true;
    }

    pub fn rename_to(&mut self, path: PathBuf) -> Result<()> {
        let current = self.path.as_ref().context("no file name to rename")?;
        fs::rename(current, &path)
            .with_context(|| format!("renaming {} to {}", current.display(), path.display()))?;
        self.path = Some(path.clone());
        self.disk_state = fingerprint_from_disk(&path)?;
        Ok(())
    }

    /// Insert `text` at byte offset `byte`. Returns the byte offset just past
    /// the inserted text (the new cursor position).
    ///
    /// `byte` must lie on a char boundary: ropey's `byte_to_char` silently
    /// floors a mid-char offset, which would shift the edit and desync the
    /// returned cursor position. The same holds for `remove` and `slice`.
    pub fn insert(&mut self, byte: usize, text: &str) -> usize {
        let char_idx = self.rope.byte_to_char(byte);
        self.rope.insert(char_idx, text);
        self.modified = true;
        byte + text.len()
    }

    /// Remove the byte range `[start, end)`.
    pub fn remove(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let s = self.rope.byte_to_char(start);
        let e = self.rope.byte_to_char(end);
        self.rope.remove(s..e);
        self.modified = true;
    }

    /// The text in byte range `[start, end)` as an owned `String`.
    pub fn slice(&self, start: usize, end: usize) -> String {
        if start >= end {
            return String::new();
        }
        let s = self.rope.byte_to_char(start);
        let e = self.rope.byte_to_char(end);
        self.rope.slice(s..e).to_string()
    }

    /// The underlying rope (read-only access for rendering).
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Total length of the buffer in bytes.
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Whether the buffer has unsaved changes.
    pub fn modified(&self) -> bool {
        self.modified
    }

    /// Override the modified flag. Undo/redo use this to clear it when the
    /// buffer returns to its last-saved state.
    pub fn set_modified(&mut self, modified: bool) {
        self.modified = modified;
    }

    /// The file's display name, or a placeholder for an unnamed buffer.
    pub fn file_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .or_else(|| self.name.clone())
            .unwrap_or_else(|| "[No Name]".to_string())
    }

    fn recovery_path(&self) -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "marqi")?;
        let identity = match self.path.as_deref() {
            Some(path) => fs::canonicalize(path)
                .or_else(|_| {
                    if path.is_absolute() {
                        Ok(path.to_path_buf())
                    } else {
                        std::env::current_dir().map(|dir| dir.join(path))
                    }
                })
                .ok()?
                .to_string_lossy()
                .into_owned(),
            None => self.name.clone().unwrap_or_else(|| "scratch".to_string()),
        };
        Some(
            dirs.cache_dir()
                .join("recovery")
                .join(format!("{:016x}.md", stable_hash(identity.as_bytes()))),
        )
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn fingerprint_from_disk(path: &Path) -> Result<DiskState> {
    match fs::read(path) {
        Ok(bytes) => fingerprint(path, &bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DiskState::Missing),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn fingerprint(path: &Path, bytes: &[u8]) -> Result<DiskState> {
    let metadata = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(DiskState::Present(DiskFingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        hash: stable_hash(bytes),
    }))
}

/// Whether CRLF is the dominant line ending in `text`.
fn detect_crlf(text: &str) -> bool {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    crlf > lf
}

/// Write `rope` to `path` atomically: stream into a sibling temp file, fsync it,
/// then rename over the target. A crash or full disk mid-write therefore never
/// truncates or corrupts an existing file — the rename either fully happens or
/// not at all. Existing file permissions are preserved across the replacement.
///
/// Known tradeoff: replacing by rename gives a file a new inode, so other hard
/// links to the old content are left behind (symlinks are handled — the path is
/// resolved first, so the link's *target* is replaced, not the link).
pub(crate) fn write_atomic(path: &Path, rope: &Rope) -> Result<()> {
    // Resolve symlinks so saving through one replaces the real file instead of
    // severing the link. A path that does not exist yet resolves to itself.
    let path = &fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("buffer");
    // Keep the temp file in the same directory so the final rename is a cheap,
    // atomic, same-filesystem operation.
    let tmp = dir.join(format!(".{stem}.marqi-{}.tmp", std::process::id()));

    let streamed = (|| -> Result<()> {
        let file = create_temp_file(&tmp, path)
            .with_context(|| format!("creating a temp file in {}", dir.display()))?;
        let mut writer = BufWriter::new(file);
        rope.write_to(&mut writer)?;
        writer.flush()?;
        // fsync so the bytes are durable before we swap the file in.
        writer
            .into_inner()
            .map_err(|e| e.into_error())?
            .sync_all()?;
        Ok(())
    })();
    if let Err(e) = streamed {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // The temp file was created with at most the target's permissions (the
    // umask may have stripped bits); now copy them exactly.
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }

    if let Err(e) = replace_file(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(anyhow::Error::new(e).context("replacing the target file"));
    }

    // Make the rename itself durable. Without this, a power loss right after
    // "saved" could roll the directory entry back to the old content. Best
    // effort: not every filesystem supports fsync on a directory.
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(&dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Create the temp file, never with broader permissions than the target: on
/// Unix it starts with the target's own mode (0666 for a new file), filtered by
/// the umask as usual — so a 0600 private file is never world-readable while
/// its content streams in.
fn create_temp_file(tmp: &Path, target: &Path) -> std::io::Result<fs::File> {
    // A stale temp file from a crashed earlier run (same PID) would make
    // `create_new` fail; it holds no precious data, so clear it.
    let _ = fs::remove_file(tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mode = fs::metadata(target)
            .map(|m| m.permissions().mode())
            .unwrap_or(0o666);
        opts.mode(mode & 0o777);
    }
    #[cfg(not(unix))]
    let _ = target;
    opts.open(tmp)
}

/// Rename `tmp` over `path`. On Windows, antivirus scanners, search indexers,
/// and sync clients briefly open files without `FILE_SHARE_DELETE`, which makes
/// the replacement fail spuriously — a short retry loop rides that out.
fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut delay = std::time::Duration::from_millis(10);
        for _ in 0..4 {
            if fs::rename(tmp, path).is_ok() {
                return Ok(());
            }
            std::thread::sleep(delay);
            delay *= 2;
        }
    }
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp path for one test (no external temp-file crate needed).
    fn temp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("marqi_test_{tag}_{}.md", std::process::id()));
        p
    }

    #[test]
    fn load_save_round_trips_bytes() {
        let path = temp_path("roundtrip");
        let original = "# Title\n\nLine with 中文 and 👨‍👩‍👧‍👦.\n";
        fs::write(&path, original).unwrap();

        let mut buf = TextBuffer::from_path(&path).unwrap();
        assert!(!buf.modified());
        buf.save().unwrap();

        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(original, after, "save must not reformat the file");

        fs::remove_file(&path).ok();
    }

    #[test]
    fn save_detects_external_changes() {
        let path = temp_path("external_change");
        fs::write(&path, "original").unwrap();
        let mut buf = TextBuffer::from_path(&path).unwrap();
        buf.insert(0, "local ");
        fs::write(&path, "external").unwrap();

        assert!(buf.has_external_change().unwrap());
        assert!(buf.save().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "external");

        buf.save_force().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "local original");
        assert!(!buf.has_external_change().unwrap());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_path_yields_empty_named_buffer() {
        let path = temp_path("missing_xyz");
        fs::remove_file(&path).ok();
        let buf = TextBuffer::from_path(&path).unwrap();
        assert_eq!(buf.rope().len_chars(), 0);
        assert_eq!(buf.file_name(), path.file_name().unwrap().to_string_lossy());
    }

    #[test]
    fn empty_buffer_has_no_name() {
        assert_eq!(TextBuffer::empty().file_name(), "[No Name]");
    }

    #[test]
    fn save_replaces_existing_file_and_leaves_no_temp() {
        let path = temp_path("atomic");
        fs::write(&path, "original").unwrap();

        let mut buf = TextBuffer::from_path(&path).unwrap();
        buf.insert(0, "X");
        buf.save().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "Xoriginal");
        // The sibling temp file is gone after the atomic rename.
        let tmp = path.parent().unwrap().join(format!(
            ".{}.marqi-{}.tmp",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        assert!(!tmp.exists(), "temp file should be renamed away");
        fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    #[test]
    fn saving_through_a_symlink_updates_the_target_and_keeps_the_link() {
        let target = temp_path("symlink_target");
        let link = temp_path("symlink_link");
        fs::write(&target, "original").unwrap();
        fs::remove_file(&link).ok();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let mut buf = TextBuffer::from_path(&link).unwrap();
        buf.insert(0, "X");
        buf.save().unwrap();

        assert!(
            fs::symlink_metadata(&link).unwrap().is_symlink(),
            "the symlink must survive the save"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "Xoriginal");
        fs::remove_file(&link).ok();
        fs::remove_file(&target).ok();
    }

    #[cfg(unix)]
    #[test]
    fn save_preserves_private_permissions_throughout() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("private_perms");
        fs::write(&path, "secret").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        let mut buf = TextBuffer::from_path(&path).unwrap();
        buf.insert(0, "X");
        buf.save().unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "private mode must survive the atomic replace");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn save_as_makes_an_unnamed_buffer_saveable() {
        let path = temp_path("saveas");
        std::fs::remove_file(&path).ok();
        let mut buf = TextBuffer::scratch("piped text", "[stdin]");
        assert!(!buf.has_path());
        buf.save_as(path.clone()).unwrap();
        assert!(buf.has_path());
        assert_eq!(fs::read_to_string(&path).unwrap(), "piped text");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn failed_save_as_keeps_the_buffer_unnamed() {
        let mut buf = TextBuffer::scratch("text", "[stdin]");
        let bad = temp_path("no_such_dir").join("sub/notes.md");
        assert!(buf.save_as(bad).is_err());
        assert!(!buf.has_path(), "a failed save-as must not bind the path");
        assert_eq!(buf.file_name(), "[stdin]");
    }

    #[test]
    fn crlf_files_insert_crlf_newlines() {
        assert_eq!(TextBuffer::scratch("a\r\nb\r\n", "[t]").newline(), "\r\n");
        assert_eq!(TextBuffer::scratch("a\nb\n", "[t]").newline(), "\n");
        assert_eq!(TextBuffer::empty().newline(), "\n");
    }

    #[test]
    fn scratch_buffer_holds_content_under_a_name() {
        let buf = TextBuffer::scratch("# Piped\n\nhi", "[stdin]");
        assert_eq!(buf.file_name(), "[stdin]");
        assert_eq!(buf.rope().to_string(), "# Piped\n\nhi");
        assert!(!buf.modified());
    }
}
