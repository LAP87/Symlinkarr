use super::*;

use std::path::PathBuf;

// ── Standard TV/Movie tests ──

#[test]
fn test_parse_standard_episode() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Breaking.Bad.S01E01.720p.BluRay.x264.mkv");
    let item = scanner.parse_filename(&path).unwrap();

    assert_eq!(item.parsed_title, "Breaking Bad");
    assert_eq!(item.season, Some(1));
    assert_eq!(item.episode, Some(1));
    assert_eq!(item.quality, Some("720p".to_string()));
    assert_eq!(item.extension, "mkv");
}

#[test]
fn test_parse_alt_episode_format() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/The.Office.1x05.Something.mkv");
    let item = scanner.parse_filename(&path).unwrap();

    assert_eq!(item.parsed_title, "The Office");
    assert_eq!(item.season, Some(1));
    assert_eq!(item.episode, Some(5));
}

#[test]
fn test_resolution_token_not_parsed_as_season_episode() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Some.Show.1920x1080.WEB-DL.mkv");
    let item = scanner.parse_filename(&path).unwrap();

    assert_eq!(item.season, None);
    assert_eq!(item.episode, None);
    assert!(item.parsed_title.starts_with("Some Show"));
}

#[test]
fn test_parse_movie() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/The.Matrix.1999.1080p.BluRay.x264.mp4");
    let item = scanner.parse_filename(&path).unwrap();

    assert_eq!(item.parsed_title, "The Matrix");
    assert_eq!(item.season, None);
    assert_eq!(item.episode, None);
    assert_eq!(item.year, Some(1999));
    assert_eq!(item.quality, Some("1080p".to_string()));
}

#[test]
fn test_parse_4k_quality() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Dune.2021.2160p.WEB-DL.mkv");
    let item = scanner.parse_filename(&path).unwrap();

    assert_eq!(item.parsed_title, "Dune");
    assert_eq!(item.quality, Some("2160p".to_string()));
    assert_eq!(item.year, Some(2021));
}

#[test]
fn test_parse_short_movie_title_with_release_year() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Up.2009.1080p.BluRay.x264.mkv");
    let item = scanner.parse_filename(&path).unwrap();

    assert_eq!(item.parsed_title, "Up");
    assert_eq!(item.year, Some(2009));
}

#[test]
fn test_non_video_extension_skipped_by_scan() {
    use tempfile::TempDir;

    let scanner = SourceScanner::new();
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("readme.txt"), "hello").unwrap();
    std::fs::write(dir.path().join("movie.mkv"), "data").unwrap();

    let source = crate::config::SourceConfig {
        name: "Test".to_string(),
        path: dir.path().to_path_buf(),
        media_type: "auto".to_string(),
    };
    let results = scanner.scan_source(&source);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].extension, "mkv");
}

#[test]
fn test_extract_title_with_underscores() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/The_Big_Bang_Theory_S01E01.mkv");
    let item = scanner.parse_filename(&path).unwrap();

    assert_eq!(item.parsed_title, "The Big Bang Theory");
}

// ── Anime parser tests ──

#[test]
fn test_anime_subgroup_bare_episode() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[SubsPlease] Jujutsu Kaisen - 03 (1080p) [ABC123].mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();

    assert_eq!(item.parsed_title, "Jujutsu Kaisen");
    assert_eq!(item.season, None);
    assert_eq!(item.episode, Some(3));
    assert_eq!(item.quality, Some("1080p".to_string()));
}

#[test]
fn test_anime_subgroup_with_inner_trailing_space_is_parsed() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[SubsPlease ]Sousou no Frieren - 01.mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();

    assert_eq!(item.parsed_title, "Sousou no Frieren");
    assert_eq!(item.season, None);
    assert_eq!(item.episode, Some(1));
    assert_eq!(item.quality, None);
}

#[test]
fn test_anime_subgroup_standard_subsplease_name_is_parsed() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[SubsPlease] Sousou no Frieren - 01.mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();

    assert_eq!(item.parsed_title, "Sousou no Frieren");
    assert_eq!(item.season, None);
    assert_eq!(item.episode, Some(1));
    assert_eq!(item.quality, None);
}

#[test]
fn test_parse_release_title_variants_for_prowlarr_titles() {
    let scanner = SourceScanner::new();
    let anime = scanner
        .parse_release_title_variants("[SubsPlease] Sousou no Frieren - 01")
        .into_iter()
        .find(|(kind, _)| *kind == ParserKind::Anime)
        .map(|(_, item)| item)
        .unwrap();

    assert_eq!(anime.parsed_title, "Sousou no Frieren");
    assert_eq!(anime.season, None);
    assert_eq!(anime.episode, Some(1));
}

#[test]
fn test_anime_standard_sxxexx() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[Erai-raws] Frieren - S01E15 [1080p].mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();

    assert_eq!(item.parsed_title, "Frieren");
    assert_eq!(item.season, Some(1));
    assert_eq!(item.episode, Some(15));
    assert_eq!(item.quality, Some("1080p".to_string()));
}

#[test]
fn test_anime_no_subgroup() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Naruto Shippuuden - 365 (1080p).mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();

    assert_eq!(item.parsed_title, "Naruto Shippuuden");
    assert_eq!(item.season, None);
    assert_eq!(item.episode, Some(365));
    assert_eq!(item.quality, Some("1080p".to_string()));
}

#[test]
fn test_anime_version_suffix() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[Judas] Vinland Saga - 03v2 (BDRip 1920x1080).mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();

    assert_eq!(item.parsed_title, "Vinland Saga");
    assert_eq!(item.episode, Some(3));
    assert_eq!(item.quality, Some("1080p".to_string()));
}

#[test]
fn test_anime_horrible_subs() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[HorribleSubs] My Hero Academia - 88 [720p].mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();

    assert_eq!(item.parsed_title, "My Hero Academia");
    assert_eq!(item.episode, Some(88));
    assert_eq!(item.quality, Some("720p".to_string()));
}

#[test]
fn test_anime_separate_season() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Title S2 - 03 (1080p).mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();

    assert_eq!(item.parsed_title, "Title");
    assert_eq!(item.season, Some(2));
    assert_eq!(item.episode, Some(3));
}

// ── Content-type dispatch test ──

#[test]
fn test_dispatch_anime_vs_tv() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[SubsPlease] Jujutsu Kaisen - 03 (1080p) [ABC123].mkv");

    let anime_item = scanner
        .parse_filename_with_type(&path, ContentType::Anime)
        .unwrap();
    assert_eq!(anime_item.parsed_title, "Jujutsu Kaisen");
    assert_eq!(anime_item.episode, Some(3));

    let tv_item = scanner
        .parse_filename_with_type(&path, ContentType::Tv)
        .unwrap();
    assert_eq!(tv_item.episode, None);
}

#[test]
fn test_parse_dual_variants_contains_anime_and_standard() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[ExampleGroup] Some Anime - 03 (1080p).mkv");
    let variants = scanner.parse_dual_variants(&path);

    assert_eq!(variants.len(), 2);

    let standard = variants
        .iter()
        .find(|(kind, _)| *kind == ParserKind::Standard)
        .map(|(_, item)| item)
        .unwrap();
    let anime = variants
        .iter()
        .find(|(kind, _)| *kind == ParserKind::Anime)
        .map(|(_, item)| item)
        .unwrap();

    assert_eq!(standard.episode, None);
    assert_eq!(anime.episode, Some(3));
}

// ── M-20: year-like season regression ──

#[test]
fn test_year_like_value_not_parsed_as_season_via_alt_format() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Some.Show.2024x01.mkv");
    let item = scanner.parse_filename(&path).unwrap();
    assert_eq!(item.season, None, "2024x01 should not parse as a season");
    assert_eq!(item.episode, None, "2024x01 should not parse as an episode");
}

// ── H-07: multi-episode parsing ──

#[test]
fn test_multi_episode_contiguous_two() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Show.S01E01E02.mkv");
    let item = scanner.parse_filename(&path).unwrap();
    assert_eq!(item.season, Some(1));
    assert_eq!(item.episode, Some(1));
    assert_eq!(item.episode_end, Some(2));
}

#[test]
fn test_multi_episode_contiguous_three() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Show.S01E01E02E03.mkv");
    let item = scanner.parse_filename(&path).unwrap();
    assert_eq!(item.season, Some(1));
    assert_eq!(item.episode, Some(1));
    assert_eq!(item.episode_end, Some(3));
}

#[test]
fn test_multi_episode_dash_two() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Show.S01E01-E02.mkv");
    let item = scanner.parse_filename(&path).unwrap();
    assert_eq!(item.season, Some(1));
    assert_eq!(item.episode, Some(1));
    assert_eq!(item.episode_end, Some(2));
}

#[test]
fn test_multi_episode_dash_range() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Show.S01E01-E03.mkv");
    let item = scanner.parse_filename(&path).unwrap();
    assert_eq!(item.season, Some(1));
    assert_eq!(item.episode, Some(1));
    assert_eq!(item.episode_end, Some(3));
}

#[test]
fn test_single_episode_has_no_episode_end() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Show.S02E05.mkv");
    let item = scanner.parse_filename(&path).unwrap();
    assert_eq!(item.season, Some(2));
    assert_eq!(item.episode, Some(5));
    assert_eq!(item.episode_end, None);
}

// ── Anime edge cases ──

#[test]
fn test_anime_bare_episode_with_subgroup_and_resolution() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[SubsPlease] Jujutsu Kaisen - 03 (720p) [ABC123].mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();
    assert_eq!(item.parsed_title, "Jujutsu Kaisen");
    assert_eq!(item.episode, Some(3));
    assert_eq!(item.quality, Some("720p".to_string()));
}

#[test]
fn test_anime_episode_v2_not_double_counted() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[Era] Show - 05v2.mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();
    assert_eq!(item.episode, Some(5));
}

#[test]
fn test_anime_quality_from_resolution_4k() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/Show - 01 (3840x2160).mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();
    assert_eq!(item.quality, Some("2160p".to_string()));
}

#[test]
fn test_anime_japanese_title_romanized() {
    let scanner = SourceScanner::new();
    let path = PathBuf::from("/mnt/rd/[Subs] Sousou no Frieren - 01 (BD 1080p).mkv");
    let item = scanner.parse_filename_anime(&path).unwrap();
    assert_eq!(item.parsed_title, "Sousou no Frieren");
    assert_eq!(item.episode, Some(1));
    assert_eq!(item.quality, Some("1080p".to_string()));
}
