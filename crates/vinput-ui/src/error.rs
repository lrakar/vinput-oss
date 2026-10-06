//! What can go wrong while showing a window or saving a picture of one.

use std::fmt;
use std::path::PathBuf;

/// Why a window could not be shown or a picture could not be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The command line asked for a canned scene that does not exist.
    UnknownScene(String),
    /// The arguments after `vinput ui` do not describe a window.
    Arguments(String),
    /// Windows refused something the window needs. `what` says which step.
    System {
        /// The step that failed, such as `creating the window`.
        what: &'static str,
        /// The code Windows gave, an `HRESULT`.
        code: i32,
    },
    /// The picture could not be written.
    Picture {
        /// Where it was to go.
        path: PathBuf,
        /// The code Windows gave, an `HRESULT`.
        code: i32,
    },
    /// This build has no windows: it is not a Windows build.
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(any(debug_assertions, test))]
            Self::UnknownScene(name) => write!(
                f,
                "no scene called {name:?}; there are {}, {} and {}",
                crate::mock::SCENES.join(", "),
                crate::scene::DIALOG_SCENES.join(", "),
                crate::gallery::SCENE
            ),
            #[cfg(not(any(debug_assertions, test)))]
            Self::UnknownScene(name) => {
                write!(
                    f,
                    "this build has no canned scenes, so none called {name:?}"
                )
            }
            Self::Arguments(what) => f.write_str(what),
            Self::System { what, code } => write!(f, "{what} failed (0x{code:08X})"),
            Self::Picture { path, code } => {
                write!(f, "writing {} failed (0x{code:08X})", path.display())
            }
            Self::Unsupported => f.write_str("the windows need Windows"),
        }
    }
}

impl std::error::Error for Error {}

/// The result of anything in this crate that can fail.
pub type Result<T> = std::result::Result<T, Error>;
