use super::*;

#[derive(Debug, Deserialize)]
struct RestoreConfigProbe {
    #[serde(default = "default_db_path")]
    db_path: String,
}

fn resolve_restore_target_path(path: &Path, base_dir: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path)
    } else {
        path.to_path_buf()
    }
}

pub fn inspect_restore_targets(config_path: &Path) -> Result<RestoreConfigTargets> {
    let config_str = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file {}", config_path.display()))?;
    let mut value: serde_yml::Value = serde_yml::from_str(&config_str)
        .with_context(|| format!("Failed to parse config file {}", config_path.display()))?;
    apply_legacy_aliases(&mut value);
    let secret_files = collect_secret_file_paths(&value, config_path.parent());
    let probe: RestoreConfigProbe = serde_yml::from_value(value).with_context(|| {
        format!(
            "Failed to inspect restore targets from config {}",
            config_path.display()
        )
    })?;

    Ok(RestoreConfigTargets {
        db_path: resolve_restore_target_path(Path::new(&probe.db_path), config_path.parent()),
        secret_files,
    })
}

pub fn candidate_config_paths(path: Option<String>) -> Vec<PathBuf> {
    if let Some(p) = path {
        return vec![PathBuf::from(p)];
    }

    let mut paths = Vec::new();
    if let Ok(env_path) = std::env::var("SYMLINKARR_CONFIG") {
        let env_path = env_path.trim();
        if !env_path.is_empty() {
            paths.push(PathBuf::from(env_path));
        }
    }
    paths.push(PathBuf::from("config.yaml"));
    paths.push(PathBuf::from("/app/config/config.yaml"));
    paths
}

pub(super) fn load_dotenv_chain(config_path: &Path) -> Result<DotenvOverlay> {
    let mut overlay = DotenvOverlay::new();
    for path in candidate_dotenv_paths(config_path) {
        if path.exists() {
            let loaded = load_dotenv_file(&path, &mut overlay)?;
            if loaded > 0 {
                tracing::info!("Loaded {} env var(s) from {:?}", loaded, path);
            }
        }
    }
    Ok(overlay)
}

fn candidate_dotenv_paths(config_path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    let mut push_unique = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };

    if let Some(config_dir) = config_path.parent() {
        push_unique(config_dir.join(".env"));
        push_unique(config_dir.join(".env.local"));
    }
    push_unique(PathBuf::from(".env"));
    push_unique(PathBuf::from(".env.local"));
    paths
}

pub(super) fn load_dotenv_file(path: &Path, overlay: &mut DotenvOverlay) -> Result<usize> {
    let content = std::fs::read_to_string(path)?;
    let mut loaded = 0usize;

    for (line_no, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            anyhow::bail!(
                "Invalid .env entry in {} at line {}",
                path.display(),
                line_no + 1
            );
        };

        let key = key.trim();
        if key.is_empty() {
            anyhow::bail!(
                "Invalid .env key in {} at line {}",
                path.display(),
                line_no + 1
            );
        }
        if std::env::var_os(key).is_some() || overlay.contains_key(key) {
            continue;
        }

        let value = parse_dotenv_value(value.trim());
        overlay.insert(key.to_string(), value);
        loaded += 1;
    }

    Ok(loaded)
}

fn parse_dotenv_value(raw: &str) -> String {
    if raw.len() >= 2 {
        let bytes = raw.as_bytes();
        let first = bytes[0];
        let last = bytes[raw.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return raw[1..raw.len() - 1].to_string();
        }
    }

    raw.to_string()
}

pub(super) fn warn_for_plaintext_secrets(root: &serde_yml::Value) {
    let plaintext_fields = raw_plaintext_secret_fields(root);
    if plaintext_fields.is_empty() {
        return;
    }

    let require_provider =
        yaml_bool_at(root, &["security", "require_secret_provider"]).unwrap_or(false);
    if require_provider {
        return;
    }

    tracing::warn!(
        "Plaintext secrets found in config for: {}. Prefer env:VAR or secretfile:/path",
        plaintext_fields.join(", ")
    );
}

pub(super) fn raw_plaintext_secret_fields(root: &serde_yml::Value) -> Vec<&'static str> {
    let mut fields = Vec::new();
    for (path, field_name) in secret_field_paths() {
        if let Some(value) = yaml_str_at(root, path) {
            if !value.is_empty() && !uses_secret_provider(value) {
                fields.push(field_name);
            }
        }
    }
    fields
}

fn yaml_value_at<'a>(root: &'a serde_yml::Value, path: &[&str]) -> Option<&'a serde_yml::Value> {
    let mut current = root;
    for segment in path {
        let mapping = current.as_mapping()?;
        current = mapping.get(serde_yml::Value::from(*segment))?;
    }
    Some(current)
}

fn yaml_str_at<'a>(root: &'a serde_yml::Value, path: &[&str]) -> Option<&'a str> {
    yaml_value_at(root, path)?.as_str()
}

fn yaml_bool_at(root: &serde_yml::Value, path: &[&str]) -> Option<bool> {
    yaml_value_at(root, path)?.as_bool()
}

pub(super) fn collect_secret_file_paths(
    root: &serde_yml::Value,
    config_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for (path, _) in secret_field_paths() {
        let Some(value) = yaml_str_at(root, path) else {
            continue;
        };
        let Some(secret_file) = value.strip_prefix("secretfile:") else {
            continue;
        };

        let secret_path = PathBuf::from(secret_file);
        let resolved = if secret_path.is_relative() {
            if let Some(config_dir) = config_dir {
                config_dir.join(secret_path)
            } else {
                secret_path
            }
        } else {
            secret_path
        };

        paths.push(resolved);
    }

    paths.sort();
    paths.dedup();
    paths
}

fn secret_field_paths() -> [(&'static [&'static str], &'static str); 16] {
    [
        (&["api", "tmdb_api_key"], "api.tmdb_api_key"),
        (
            &["api", "tmdb_read_access_token"],
            "api.tmdb_read_access_token",
        ),
        (&["api", "tvdb_api_key"], "api.tvdb_api_key"),
        (&["realdebrid", "api_token"], "realdebrid.api_token"),
        (&["decypharr", "api_token"], "decypharr.api_token"),
        (&["prowlarr", "api_key"], "prowlarr.api_key"),
        (&["bazarr", "api_key"], "bazarr.api_key"),
        (&["tautulli", "api_key"], "tautulli.api_key"),
        (&["plex", "token"], "plex.token"),
        (&["emby", "api_key"], "emby.api_key"),
        (&["jellyfin", "api_key"], "jellyfin.api_key"),
        (&["web", "password"], "web.password"),
        (&["web", "api_key"], "web.api_key"),
        (&["radarr", "api_key"], "radarr.api_key"),
        (&["sonarr", "api_key"], "sonarr.api_key"),
        (&["sonarr_anime", "api_key"], "sonarr_anime.api_key"),
    ]
}

pub(super) fn apply_legacy_aliases(root: &mut serde_yml::Value) {
    let Some(mapping) = root.as_mapping_mut() else {
        return;
    };

    let backup_key = serde_yml::Value::from("backup");
    let Some(backup_value) = mapping.get_mut(&backup_key) else {
        return;
    };
    let Some(backup_map) = backup_value.as_mapping_mut() else {
        return;
    };

    let path_key = serde_yml::Value::from("path");
    let dir_key = serde_yml::Value::from("dir");
    if !backup_map.contains_key(&path_key) {
        if let Some(dir_value) = backup_map.get(&dir_key).cloned() {
            backup_map.insert(path_key, dir_value);
            tracing::warn!(
                "Deprecated config key 'backup.dir' detected; please migrate to 'backup.path'"
            );
        }
    }
}

pub(super) fn resolve_secret(
    raw: &str,
    field: &str,
    require_provider: bool,
    config_dir: Option<&Path>,
    dotenv_overlay: Option<&DotenvOverlay>,
) -> Result<String> {
    if raw.is_empty() {
        return Ok(String::new());
    }

    if let Some(var) = raw.strip_prefix("env:") {
        let value = std::env::var(var)
            .ok()
            .or_else(|| dotenv_overlay.and_then(|overlay| overlay.get(var).cloned()))
            .ok_or_else(|| {
                anyhow::anyhow!("Missing environment variable '{}' for {}", var, field)
            })?;
        return Ok(value.trim().to_string());
    }

    if let Some(file) = raw.strip_prefix("secretfile:") {
        let file_path = PathBuf::from(file);
        let resolved_path = if file_path.is_relative() {
            if let Some(config_dir) = config_dir {
                config_dir.join(file_path)
            } else {
                file_path
            }
        } else {
            file_path
        };
        let value = std::fs::read_to_string(&resolved_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed reading secret file '{}' for {}: {}",
                resolved_path.display(),
                field,
                e
            )
        })?;
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!(
                "Secret file '{}' is empty or contains only whitespace",
                resolved_path.display()
            );
        }
        return Ok(trimmed);
    }

    if require_provider {
        anyhow::bail!(
            "Plaintext secret is not allowed for {}. Use env:VAR or secretfile:/path/to/file",
            field
        );
    }

    Ok(raw.to_string())
}

pub(super) fn uses_secret_provider(raw: &str) -> bool {
    raw.starts_with("env:") || raw.starts_with("secretfile:")
}
