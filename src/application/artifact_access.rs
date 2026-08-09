use tokio::fs;

use crate::config::LocalArtifactConfig;
use crate::error::{AppError, AppResult, ErrorCode};

/// Resolve one caller-supplied artifact path against the configured local
/// artifact roots before any artifact content is opened or hashed.
///
/// The returned path is canonical and therefore is also the path delegated to
/// remote-exec-mcp for upload. remote-exec-mcp must still enforce its own local
/// transfer policy; this check protects deploy-mcp's independent local read.
pub(crate) async fn resolve_allowed_artifact_path(
    policy: &LocalArtifactConfig,
    requested_path: &str,
) -> AppResult<String> {
    if requested_path.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::InvalidRequest,
            "artifact_path must not be empty",
        ));
    }
    if policy.allowed_roots.is_empty() {
        return Err(AppError::new(
            ErrorCode::ArtifactPathNotAllowed,
            "local artifact access is disabled because no allowed roots are configured",
        ));
    }

    let mut canonical_roots = Vec::with_capacity(policy.allowed_roots.len());
    for configured_root in &policy.allowed_roots {
        let canonical_root = fs::canonicalize(configured_root).await.map_err(|error| {
            AppError::invalid_configuration(format!(
                "cannot resolve local artifact allowed root {configured_root}: {error}"
            ))
        })?;
        let metadata = fs::metadata(&canonical_root).await.map_err(|error| {
            AppError::invalid_configuration(format!(
                "cannot stat local artifact allowed root {}: {error}",
                canonical_root.display()
            ))
        })?;
        if !metadata.is_dir() {
            return Err(AppError::invalid_configuration(format!(
                "local artifact allowed root must be a directory: {}",
                canonical_root.display()
            )));
        }
        if canonical_root.parent().is_none() {
            return Err(AppError::invalid_configuration(format!(
                "canonical local artifact allowed root must not resolve to a filesystem root: {}",
                canonical_root.display()
            )));
        }
        canonical_roots.push(canonical_root);
    }

    let canonical_artifact = fs::canonicalize(requested_path).await.map_err(|error| {
        AppError::new(
            ErrorCode::ArtifactNotFound,
            format!("cannot resolve artifact {requested_path}: {error}"),
        )
    })?;

    if !canonical_roots
        .iter()
        .any(|root| canonical_artifact.starts_with(root))
    {
        return Err(AppError::new(
            ErrorCode::ArtifactPathNotAllowed,
            format!(
                "artifact path is outside configured local artifact roots: {}",
                canonical_artifact.display()
            ),
        ));
    }

    canonical_artifact
        .into_os_string()
        .into_string()
        .map_err(|_| {
            AppError::new(
                ErrorCode::InvalidArtifact,
                "canonical artifact path is not valid UTF-8",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_allowlist_denies_before_filesystem_lookup() {
        let policy = LocalArtifactConfig::default();
        let error = resolve_allowed_artifact_path(&policy, "/definitely/missing/demo.jar")
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ArtifactPathNotAllowed);
    }

    #[tokio::test]
    async fn file_below_allowed_root_resolves_to_canonical_path() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("demo.jar");
        std::fs::write(&artifact, b"jar").unwrap();
        let policy = LocalArtifactConfig {
            allowed_roots: vec![directory.path().to_string_lossy().into_owned()],
        };

        let resolved = resolve_allowed_artifact_path(&policy, &artifact.to_string_lossy())
            .await
            .unwrap();
        assert_eq!(
            std::path::PathBuf::from(resolved),
            std::fs::canonicalize(artifact).unwrap()
        );
    }

    #[tokio::test]
    async fn file_outside_allowed_root_is_denied() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let artifact = outside.path().join("demo.jar");
        std::fs::write(&artifact, b"jar").unwrap();
        let policy = LocalArtifactConfig {
            allowed_roots: vec![allowed.path().to_string_lossy().into_owned()],
        };

        let error = resolve_allowed_artifact_path(&policy, &artifact.to_string_lossy())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ArtifactPathNotAllowed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_inside_allowed_root_cannot_escape_to_outside_file() {
        use std::os::unix::fs::symlink;

        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_artifact = outside.path().join("outside.jar");
        std::fs::write(&outside_artifact, b"jar").unwrap();
        let link = allowed.path().join("inside.jar");
        symlink(&outside_artifact, &link).unwrap();
        let policy = LocalArtifactConfig {
            allowed_roots: vec![allowed.path().to_string_lossy().into_owned()],
        };

        let error = resolve_allowed_artifact_path(&policy, &link.to_string_lossy())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ArtifactPathNotAllowed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_allowed_root_cannot_canonicalize_to_filesystem_root() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("root-link");
        symlink("/", &link).unwrap();
        let policy = LocalArtifactConfig {
            allowed_roots: vec![link.to_string_lossy().into_owned()],
        };

        let error = resolve_allowed_artifact_path(&policy, "/definitely/missing/demo.jar")
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error.message.contains("filesystem root"));
    }
}
