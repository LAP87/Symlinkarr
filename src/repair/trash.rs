use super::*;

pub(super) fn parse_trash_filename(filename: &str) -> TrashMeta {
    // Strip extension
    let name = filename
        .rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(filename);

    // Extract IMDB ID: {imdb-tt1234567}
    let imdb_id = trash_imdb_regex().captures(name).map(|c| c[1].to_string());

    // Extract season/episode: S01E03
    let (season, episode) = trash_season_episode_regex()
        .captures(name)
        .map(|c| (c[1].parse::<u32>().ok(), c[2].parse::<u32>().ok()))
        .unwrap_or((None, None));

    // Extract quality: [WEBDL-1080p], [Bluray-2160p], or standalone 1080p/2160p/720p
    let quality = trash_quality_regex().captures(name).map(|c| {
        let res = c.get(1).or(c.get(2)).unwrap().as_str();
        format!("{}p", res)
    });

    // Extract year: (2008) — first 4-digit number in parentheses
    let year = trash_year_regex()
        .captures(name)
        .and_then(|c| c[1].parse::<u32>().ok());

    // Extract title: everything before the first (year), S01E, or [quality] marker
    let title = if let Some(m) = trash_title_end_regex().find(name) {
        name[..m.start()]
            .trim()
            .trim_end_matches(" -")
            .trim()
            .to_string()
    } else {
        name.trim().to_string()
    };

    TrashMeta {
        title,
        year,
        season,
        episode,
        quality,
        imdb_id,
    }
}

fn tagged_media_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{(tvdb|tmdb)-([0-9]+)\}").unwrap())
}

pub(super) fn extract_media_id_from_tagged_ancestors(
    path: &Path,
    library_root: &Path,
) -> Option<String> {
    for ancestor in path.ancestors() {
        if ancestor == library_root {
            break;
        }

        let folder_name = match ancestor.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => continue,
        };
        if let Some(captures) = tagged_media_id_regex().captures(folder_name) {
            return Some(format!("{}-{}", &captures[1], &captures[2]));
        }
    }

    None
}

fn trash_imdb_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{imdb-(tt\d+)\}").unwrap())
}

pub(super) fn trash_season_episode_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[Ss](\d{1,2})[Ee](\d{1,3})").unwrap())
}

pub(super) fn trash_quality_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(?:\[(?:[\w\s-]*?)?(2160|1080|720|480)p[^\]]*\]|(2160|1080|720|480)p)")
            .unwrap()
    })
}

fn trash_year_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\((\d{4})\)").unwrap())
}

fn trash_title_end_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(\(\d{4}\)|[Ss]\d{1,2}[Ee]|\[|\{imdb-)").unwrap())
}
