use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::media_preview::MediaPreview;

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallpaperCandidate {
    pub id: String,
    pub source: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    pub media_preview: MediaPreview,
}

#[derive(Debug, Clone)]
pub struct LocalProvider {
    home: PathBuf,
}

impl LocalProvider {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    pub fn from_environment() -> Result<Self, io::Error> {
        env::var_os("HOME")
            .map(Self::new)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not available"))
    }

    pub fn discover(&self, paths: &[String]) -> Result<Vec<WallpaperCandidate>, io::Error> {
        let mut files = BTreeSet::new();
        let mut visited_directories = BTreeSet::new();
        for path in paths {
            self.walk(
                &self.expand_tilde(path),
                &mut visited_directories,
                &mut files,
            )?;
        }
        files
            .into_iter()
            .map(|path| candidate_from_path(&path))
            .collect()
    }

    fn expand_tilde(&self, input: &str) -> PathBuf {
        if input == "~" {
            return self.home.clone();
        }
        input
            .strip_prefix("~/")
            .map(|rest| self.home.join(rest))
            .unwrap_or_else(|| PathBuf::from(input))
    }

    fn walk(
        &self,
        path: &Path,
        visited_directories: &mut BTreeSet<PathBuf>,
        files: &mut BTreeSet<PathBuf>,
    ) -> Result<(), io::Error> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if metadata.is_file() {
            if is_supported_image(path) {
                files.insert(fs::canonicalize(path)?);
            }
            return Ok(());
        }
        if !metadata.is_dir() {
            return Ok(());
        }

        let canonical = fs::canonicalize(path)?;
        if !visited_directories.insert(canonical) {
            return Ok(());
        }
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            self.walk(&entry.path(), visited_directories, files)?;
        }
        Ok(())
    }
}

fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
}

fn candidate_from_path(path: &Path) -> Result<WallpaperCandidate, io::Error> {
    let path_string = path.to_string_lossy().into_owned();
    let mime = mime_for_path(path).map(str::to_owned);
    let size = fs::metadata(path).ok().map(|metadata| metadata.len());
    Ok(WallpaperCandidate {
        id: path_string.clone(),
        source: "local".into(),
        path: path_string.clone(),
        mime: mime.clone(),
        media_preview: MediaPreview::local_image(path_string, mime, size),
    })
}

fn mime_for_path(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn discovers_supported_images_recursively_with_local_previews() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = env::temp_dir().join(format!("kitowall-local-{id}"));
        let nested = home.join("Pictures/nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(home.join("Pictures/a.JPG"), b"image").unwrap();
        fs::write(nested.join("b.webp"), b"image").unwrap();
        fs::write(nested.join("ignore.txt"), b"text").unwrap();

        let candidates = LocalProvider::new(&home)
            .discover(&["~/Pictures".into()])
            .unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].source, "local");
        assert_eq!(candidates[0].media_preview.schema_version, 1);
        assert!(candidates.iter().all(|candidate| candidate
            .media_preview
            .source
            .local_path
            .is_some()));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn ignores_missing_roots() {
        let provider = LocalProvider::new("/tmp/kitowall-nonexistent-home");
        assert!(provider.discover(&["~/missing".into()]).unwrap().is_empty());
    }
}
