//! Where the viewer was, so it comes back there: the picture on screen and
//! how it was viewed (rotation, zoom, the point at the window's center).
//! The picture is saved whenever one opens, the view when the window exits
//! (`on_exit`), and a launch opens on them — after a restart (the remote's
//! "Restart focused app", `ccectl restart-app`), at login, or when reopened.
//!
//! **A restart replays the same command line**, so a viewer opened as
//! `cce-image a.png` would always come back on `a.png`. The file therefore
//! also records the files the command line named (`launched`): the same
//! command line again is this window reopening, and the saved picture wins
//! (even one opened since); a command line naming other files is a new
//! request, and it wins; none at all reopens the saved picture. A picture
//! that no longer exists is never reopened.
//!
//! `cce_state_dir()/image/state.json`, written whole to a temporary file and
//! renamed over, so a crash mid-write leaves the last one.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// The picture on screen, absolute.
    pub path: PathBuf,
    pub view: View,
    /// The files the command line named when this was saved, absolute.
    pub launched: Vec<PathBuf>,
}

/// How the picture was viewed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct View {
    /// User rotation in quarter turns clockwise.
    pub quarter_turns: u8,
    /// Fitted to the window (then `zoom` and `center` are not used).
    pub fit: bool,
    /// Screen px per source px.
    pub zoom: f64,
    /// The picture point at the window's center, as a fraction of the
    /// (rotated) picture, so it survives a different window size.
    pub center: (f64, f64),
}

impl Default for View {
    fn default() -> Self {
        View { quarter_turns: 0, fit: true, zoom: 1.0, center: (0.5, 0.5) }
    }
}

/// The files this run's command line named, absolute.
static LAUNCHED: OnceLock<Vec<PathBuf>> = OnceLock::new();

fn path() -> PathBuf {
    cce_ui::config::cce_state_dir().join("image").join("state.json")
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// What to reopen, if anything: the saved picture when this command line
/// is the one it was saved under or names nothing; else `None`, and the
/// command line is honoured.
pub fn initial(argv: &[PathBuf]) -> Option<Saved> {
    let launched: Vec<PathBuf> = argv.iter().map(|p| absolute(p)).collect();
    let _ = LAUNCHED.set(launched.clone());
    let saved = std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str::<Saved>(&s).ok());
    choose(&launched, saved.as_ref(), |p| p.is_file()).cloned()
}

fn choose<'a>(launched: &[PathBuf], saved: Option<&'a Saved>, exists: impl Fn(&Path) -> bool) -> Option<&'a Saved> {
    saved.filter(|s| launched.is_empty() || s.launched == launched).filter(|s| exists(&s.path))
}

/// The picture on screen, or how it is viewed, changed.
pub fn save(picture: &Path, view: View) {
    let saved = Saved { path: absolute(picture), view, launched: LAUNCHED.get().cloned().unwrap_or_default() };
    let path = path();
    let Some(dir) = path.parent() else { return };
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&tmp, serde_json::to_vec(&saved).unwrap_or_default()))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        log::warn!("cannot save the open picture to {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(path: &str, launched: &[&str]) -> Saved {
        Saved { path: path.into(), view: View::default(), launched: launched.iter().map(PathBuf::from).collect() }
    }

    fn argv(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    const ALL_EXIST: fn(&Path) -> bool = |_| true;

    #[test]
    fn the_same_command_line_reopens_where_it_was() {
        // Launched on a.png, moved on to b.png, restarted with the same argv.
        let s = saved("/p/b.png", &["/p/a.png"]);
        assert_eq!(choose(&argv(&["/p/a.png"]), Some(&s), ALL_EXIST), Some(&s));
        let s = saved("/p/b.png", &["/p/a.png", "/p/c.png"]);
        assert_eq!(choose(&argv(&["/p/a.png", "/p/c.png"]), Some(&s), ALL_EXIST), Some(&s));
    }

    #[test]
    fn a_plain_launch_reopens_whatever_was_saved() {
        let s = saved("/p/b.png", &[]);
        assert_eq!(choose(&[], Some(&s), ALL_EXIST), Some(&s));
        let s = saved("/p/b.png", &["/p/a.png"]);
        assert_eq!(choose(&[], Some(&s), ALL_EXIST), Some(&s));
    }

    #[test]
    fn a_command_line_naming_other_files_wins() {
        let s = saved("/p/b.png", &["/p/a.png"]);
        assert_eq!(choose(&argv(&["/p/c.png"]), Some(&s), ALL_EXIST), None);
        let s = saved("/p/b.png", &[]);
        assert_eq!(choose(&argv(&["/p/b.png"]), Some(&s), ALL_EXIST), None);
        let s = saved("/p/b.png", &["/p/a.png"]);
        assert_eq!(choose(&argv(&["/p/a.png", "/p/c.png"]), Some(&s), ALL_EXIST), None);
    }

    #[test]
    fn nothing_saved_or_a_missing_picture_restores_nothing() {
        assert_eq!(choose(&[], None, ALL_EXIST), None);
        assert_eq!(choose(&argv(&["/p/a.png"]), None, ALL_EXIST), None);
        let s = saved("/p/gone.png", &["/p/a.png"]);
        let exists = |p: &Path| p != Path::new("/p/gone.png");
        assert_eq!(choose(&argv(&["/p/a.png"]), Some(&s), exists), None);
        assert_eq!(choose(&[], Some(&s), exists), None);
    }

    #[test]
    fn the_file_round_trips() {
        let s = Saved {
            path: "/p/b.png".into(),
            view: View { quarter_turns: 3, fit: false, zoom: 2.5, center: (0.25, 0.75) },
            launched: argv(&["/p/a.png"]),
        };
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Saved>(&text).unwrap(), s);
    }
}
